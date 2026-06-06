//! LSP glue — bridges `App` state with `hjkl_lsp::LspManager`.

use std::path::PathBuf;

use hjkl_buffer_tui::Sign;
use ratatui::style::{Color, Modifier, Style};
use serde_json::json;

use crate::completion::{Completion, item_from_lsp};

use super::{App, DiagSeverity, LspDiag, LspPendingRequest, LspServerInfo};

/// Default timeout before the sweep drops an unanswered pending request
/// (clearing the status spinner). Generous enough for a cold rust-analyzer
/// goto, short enough that a dead/misconfigured server doesn't spin forever.
const LSP_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Shorter timeout for auto-fired completion: it should feel instant, and the
/// popup already shows buffer words, so an unresponsive server (e.g. taplo on
/// TOML) shouldn't keep the spinner lit for long.
const LSP_AUTO_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Per-request timeout used by the stale-pending sweep.
fn pending_request_timeout(req: &LspPendingRequest) -> std::time::Duration {
    match req {
        LspPendingRequest::Completion { auto: true, .. } => LSP_AUTO_COMPLETION_TIMEOUT,
        _ => LSP_REQUEST_TIMEOUT,
    }
}

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

/// JSON-pointer to the server-capability that gates an LSP request `method`,
/// or `None` for methods that need no capability check (e.g. notifications or
/// `workspace/executeCommand`). Used to skip requests a server can't service.
fn capability_pointer_for_method(method: &str) -> Option<&'static str> {
    Some(match method {
        "textDocument/definition" => "/definitionProvider",
        "textDocument/declaration" => "/declarationProvider",
        "textDocument/typeDefinition" => "/typeDefinitionProvider",
        "textDocument/implementation" => "/implementationProvider",
        "textDocument/references" => "/referencesProvider",
        "textDocument/hover" => "/hoverProvider",
        "textDocument/completion" => "/completionProvider",
        "textDocument/codeAction" => "/codeActionProvider",
        "textDocument/rename" => "/renameProvider",
        "textDocument/formatting" => "/documentFormattingProvider",
        _ => return None,
    })
}

/// Snap `byte` down to the nearest char boundary in `rope`.
///
/// LSP byte offsets that are clamped to `len_bytes` can land in the middle of
/// a multi-byte char (e.g. a 4-byte emoji whose last byte is at position N-1
/// while `len_bytes` clamps to N-3). `ropey::Rope::byte_slice` panics on a
/// non-aligned range; this helper floors the byte index to the start of the
/// char that contains it using ropey's own conversion, which is safe on any
/// byte value ≤ len_bytes (the docs guarantee `byte_to_char` returns the char
/// index of the char *containing* that byte when it is a non-boundary byte).
fn snap_to_char_boundary(rope: &ropey::Rope, byte: usize) -> usize {
    let b = byte.min(rope.len_bytes());
    rope.char_to_byte(rope.byte_to_char(b))
}

/// Build the `TextChange[]` array for `textDocument/didChange` incremental
/// sync from the engine's `ContentEdit` batch.
///
/// LSP positions are interpreted relative to the document state *before*
/// each change is applied (and *after* every preceding change in the same
/// array). Our `ContentEdit`s were recorded against the buffer's evolving
/// state during the edit run, so they already satisfy this contract — we
/// just need the replacement text. Slice it directly from the rope via
/// `byte_slice(start..end).to_string()`: ropey returns a `RopeSlice` in
/// O(log N) and converts only the slice's bytes to a `String` — no
/// document-wide allocation. Replaces the prior path which forced a
/// full `content_joined()` build (~3 MB on a 1.86 M-line file, ~15 % of
/// per-keystroke CPU when LSP was attached).
///
/// Caller MUST verify the server uses UTF-8 `positionEncoding`; this
/// function passes byte columns straight through.
fn build_text_changes(
    rope: &ropey::Rope,
    edits: &[hjkl_engine::ContentEdit],
) -> Vec<hjkl_lsp::TextChange> {
    let len_bytes = rope.len_bytes();
    edits
        .iter()
        .map(|e| {
            // Clamp then snap to char boundaries: `.min(len_bytes)` can land
            // in the middle of a multi-byte char (e.g. on emoji/CJK content),
            // causing `byte_slice` to panic. `snap_to_char_boundary` floors
            // each bound to the start of the char that contains it.
            let start = snap_to_char_boundary(rope, e.start_byte.min(len_bytes));
            let end = snap_to_char_boundary(rope, e.new_end_byte.min(len_bytes)).max(start);
            let text = rope.byte_slice(start..end).to_string();
            hjkl_lsp::TextChange {
                start_line: e.start_position.0,
                start_col: e.start_position.1,
                end_line: e.old_end_position.0,
                end_col: e.old_end_position.1,
                text,
            }
        })
        .collect()
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

        // Drop pending requests whose server exited or never answered, so the
        // "LSP:…" status spinner can't hang forever (e.g. a misconfigured TOML
        // server that exits without responding).
        self.sweep_stale_lsp_pending_at(std::time::Instant::now());
    }

    /// Stamp newly-seen pending requests with `now`, then drop any that have
    /// outlived their per-request timeout (or whose id is no longer pending).
    /// Split from the wall-clock so it can be unit-tested with a controlled
    /// clock.
    pub(crate) fn sweep_stale_lsp_pending_at(&mut self, now: std::time::Instant) {
        // Record first-sight time for any request not yet tracked.
        let ids: Vec<i64> = self.lsp_pending.keys().copied().collect();
        for id in ids {
            self.lsp_pending_seen_at.entry(id).or_insert(now);
        }
        // Collect ids to drop: resolved (no longer pending) or timed out.
        let mut drop_ids: Vec<i64> = Vec::new();
        for (id, seen) in self.lsp_pending_seen_at.iter() {
            let timed_out = match self.lsp_pending.get(id) {
                Some(req) => now.saturating_duration_since(*seen) >= pending_request_timeout(req),
                None => true, // already resolved — clean up its timestamp
            };
            if timed_out {
                drop_ids.push(*id);
            }
        }
        for id in drop_ids {
            if self.lsp_pending.remove(&id).is_some() {
                tracing::warn!(request_id = id, "lsp request timed out; dropping pending");
            }
            self.lsp_pending_seen_at.remove(&id);
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
        let diag_signs_lsp: Vec<Sign> = sign_map
            .into_iter()
            .map(|(row, (_, ch, style, priority))| Sign {
                row,
                ch,
                style,
                priority,
            })
            .collect();

        tracing::debug!(
            slot = slot_idx,
            n_diags = lsp_diags.len(),
            n_signs = diag_signs_lsp.len(),
            "lsp publishDiagnostics applied"
        );
        let slot = &mut self.slots[slot_idx];
        slot.lsp_diags = lsp_diags;
        slot.diag_signs_lsp = diag_signs_lsp;
    }

    /// Send a `textDocument/didChange` notification for the active buffer,
    /// but only when the buffer's `dirty_gen` has advanced since the last
    /// send. This naturally batches rapid keystroke edits.
    ///
    /// `edits` is the batch of [`hjkl_engine::ContentEdit`]s that produced
    /// the new buffer state — same list passed to `syntax.apply_edits`. When
    /// non-empty and the active server supports incremental sync over
    /// UTF-8 positions, sends an incremental `contentChanges` array (one
    /// element per edit, with replacement text sliced from the post-edit
    /// `content_joined()` cache). Otherwise falls back to full-document
    /// sync — the only choice when the buffer was wholesale-replaced
    /// (`:e!` / formatter), the server uses a non-UTF-8 position
    /// encoding, or no edits are tracked.
    pub(crate) fn lsp_notify_change_active(&mut self, edits: &[hjkl_engine::ContentEdit]) {
        if self.lsp.as_ref().is_none() {
            return;
        }
        // Compute sync-mode decision before taking the mutable slot
        // borrow — `lsp_supports_incremental_utf8` walks `self.lsp_state`.
        let use_incremental = !edits.is_empty() && self.lsp_supports_incremental_utf8();

        let mgr = self.lsp.as_ref().unwrap();
        let slot_idx = self.focused_slot_idx();
        let slot = &mut self.slots[slot_idx];
        let dg = slot.editor.buffer().dirty_gen();

        // Skip if dirty_gen unchanged since last send.
        if slot.last_lsp_dirty_gen == Some(dg) {
            return;
        }
        slot.last_lsp_dirty_gen = Some(dg);

        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;

        if use_incremental {
            // Slice per-edit text directly from the rope — avoids the
            // ~3 MB content_joined build that dominated the LSP path on
            // huge files. `Buffer::rope()` is an O(1) Arc-clone.
            let rope = slot.editor.buffer().rope();
            let changes = build_text_changes(&rope, edits);
            tracing::debug!(
                buffer_id,
                dg,
                n_changes = changes.len(),
                "lsp didChange incremental"
            );
            mgr.notify_change_incremental(buffer_id, changes);
        } else {
            // Full-sync fallback (server doesn't support incremental,
            // or `:e!` / formatter wiped the edit log): we still need
            // the whole document, so pay for `content_joined` here.
            let text = slot.editor.buffer().content_joined();
            tracing::debug!(
                buffer_id,
                dg,
                n_edits = edits.len(),
                text_len = text.len(),
                "lsp didChange full"
            );
            mgr.notify_change(buffer_id, text);
        }
    }

    /// Notify the LSP server that the buffer in `slot_idx` was saved. Flushes
    /// the current (post-format-on-save) text with a full `didChange` first so
    /// the server flychecks exactly what was written, then sends `didSave` —
    /// which is what triggers rust-analyzer's `cargo clippy` run. No-op when LSP
    /// is disabled or the buffer isn't attached to a server.
    pub(crate) fn lsp_notify_save_slot(&mut self, slot_idx: usize) {
        if self.lsp.is_none() {
            return;
        }
        let Some(slot) = self.slots.get(slot_idx) else {
            return;
        };
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
        let text = slot.editor.buffer().content_joined();
        let dg = slot.editor.buffer().dirty_gen();
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.notify_change(buffer_id, text);
            mgr.notify_save(buffer_id);
        }
        // Record the synced gen so the next edit's incremental didChange isn't
        // a redundant resend of the same state.
        self.slots[slot_idx].last_lsp_dirty_gen = Some(dg);
    }

    /// True when the active buffer's LSP server announced both incremental
    /// sync (`textDocumentSync.change == 2`) and UTF-8 `positionEncoding`.
    /// Falls back conservatively when capabilities are missing.
    fn lsp_supports_incremental_utf8(&self) -> bool {
        // Find the server attached to the active buffer's language.
        let lang = match self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext)
        {
            Some(l) => l,
            None => return false,
        };
        let info = match self.lsp_state.iter().find(|(k, _)| k.language == lang) {
            Some((_, info)) => info,
            None => return false,
        };
        let caps = &info.capabilities;

        // positionEncoding default per spec is "utf-16" — require explicit utf-8.
        let pos_enc = caps
            .get("positionEncoding")
            .and_then(|v| v.as_str())
            .unwrap_or("utf-16");
        if pos_enc != "utf-8" {
            return false;
        }

        // textDocumentSync.change == 2 = Incremental.
        // The field can be either an integer (legacy shape) or a
        // `TextDocumentSyncOptions` object with a `change` integer.
        let sync = caps.get("textDocumentSync");
        let change_kind = sync
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.get("change").and_then(|c| c.as_i64()))
            })
            .unwrap_or(0);
        change_kind == 2
    }

    /// True when an *initialized* LSP server for the active buffer's language
    /// advertises the capability at `pointer` (a JSON pointer such as
    /// `"/completionProvider"`). A capability explicitly set to `false` counts
    /// as unsupported.
    ///
    /// Gating requests on this prevents queuing a pending request that the
    /// server will never answer — either because it hasn't finished its
    /// `initialize` handshake or because it doesn't implement the feature.
    /// Without it, an auto-fired request (e.g. completion on every keystroke to
    /// a TOML server with no completion support) leaves the "LSP:…" status
    /// spinner stuck.
    fn lsp_active_supports(&self, pointer: &str) -> bool {
        let Some(lang) = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext)
        else {
            return false;
        };
        self.lsp_state.iter().any(|(k, info)| {
            k.language == lang
                && info.initialized
                && info
                    .capabilities
                    .pointer(pointer)
                    .is_some_and(|v| *v != serde_json::Value::Bool(false))
        })
    }

    /// File-type label for the active buffer — the language id string when
    /// the extension is recognized, otherwise the raw extension, otherwise
    /// `"(none)"`.
    ///
    /// Used by the status-line right-click menu to show `Filetype: rust`.
    pub(crate) fn active_filetype_label(&self) -> String {
        let ext = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext.is_empty() {
            return "(none)".to_string();
        }
        match language_id_for_ext(ext) {
            Some(lang) => lang.to_string(),
            None => ext.to_string(),
        }
    }

    /// Single-line comment lead for the active buffer's language (e.g. `"//"`
    /// for Rust/JS/C, `"#"` for Python/shell, `"--"` for Lua/SQL). Used to
    /// prefix end-of-line ghost-text hints (inline blame, diagnostics) so they
    /// read like a trailing comment. Falls back to `"//"` for unknown
    /// languages.
    pub(crate) fn active_comment_lead(&self) -> &'static str {
        self.active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext)
            .and_then(hjkl_lang::comment::commentstring_for_lang)
            .map(|(start, _)| start)
            .unwrap_or("//")
    }

    /// LSP server name for the active buffer, if one is attached.
    ///
    /// Returns the `language` string from the first matching [`ServerKey`]
    /// in `lsp_state` (which doubles as the server name in the simple
    /// one-server-per-language setup used today).
    pub(crate) fn active_lsp_server_name(&self) -> Option<String> {
        self.lsp.as_ref()?;
        let lang = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext)?;
        self.lsp_state
            .keys()
            .find(|k| k.language == lang)
            .map(|k| k.language.clone())
    }

    /// Restart the LSP server for the active buffer: detach, then re-attach.
    ///
    /// Detaching stops the server and clears state; re-attaching restarts it.
    /// Mirrors what a `:LspRestart` command would do.
    pub(crate) fn restart_lsp(&mut self) {
        let slot_idx = self.focused_slot_idx();
        self.lsp_detach_buffer(slot_idx);
        self.lsp_attach_buffer(slot_idx);
        self.bus.info("LSP restarted");
    }

    /// Return `true` when the active buffer has a running, initialized LSP
    /// server attached (i.e. its file extension maps to a configured language
    /// and the server has completed the `initialize` handshake).
    ///
    /// Used by the right-click context menu to enable/disable LSP items.
    pub(crate) fn active_has_lsp(&self) -> bool {
        if self.lsp.is_none() {
            return false;
        }
        let lang = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext);
        let Some(lang) = lang else {
            return false;
        };
        self.lsp_state.keys().any(|k| k.language == lang)
    }

    /// Attach `slot_idx` to the appropriate language server (if configured).
    pub(crate) fn lsp_attach_buffer(&mut self, slot_idx: usize) {
        if !self.slots[slot_idx].features.lsp {
            return;
        }
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

        // `content_joined()` returns a `dirty_gen`-cached `Arc<String>`,
        // shared with any other per-tick consumer. Beats `lines().join`,
        // which clones every row out of the rope.
        let text = self.slots[slot_idx].editor.buffer().content_joined();

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
    /// `extras` is merged into the top-level params object before sending —
    /// most goto methods take only `TextDocumentPositionParams`, but
    /// `textDocument/references` requires an additional `context` field
    /// (`{ includeDeclaration: bool }`) per the LSP spec.
    fn lsp_send_goto(
        &mut self,
        method: &str,
        extras: Option<serde_json::Value>,
        make_pending: impl FnOnce(hjkl_lsp::BufferId, (usize, usize)) -> LspPendingRequest,
    ) {
        if self.lsp.is_none() {
            self.bus
                .error("LSP: not enabled (set [lsp] enabled = true in config)");
            return;
        }
        if let Some(ptr) = capability_pointer_for_method(method)
            && !self.lsp_active_supports(ptr)
        {
            self.bus
                .error(format!("LSP: server does not support {method}"));
            return;
        }
        let (mut params, buffer_id, origin) = match self.lsp_position_params() {
            Some(v) => v,
            None => {
                self.bus.error(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)",
                );
                return;
            }
        };
        if let (Some(extra), Some(obj)) = (extras, params.as_object_mut())
            && let Some(extra_obj) = extra.as_object()
        {
            for (k, v) in extra_obj {
                obj.insert(k.clone(), v.clone());
            }
        }
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
        self.lsp_send_goto("textDocument/definition", None, |buf, orig| {
            LspPendingRequest::GotoDefinition {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gD` — goto declaration.
    pub(crate) fn lsp_goto_declaration(&mut self) {
        self.lsp_send_goto("textDocument/declaration", None, |buf, orig| {
            LspPendingRequest::GotoDeclaration {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gy` — goto type definition.
    pub(crate) fn lsp_goto_type_definition(&mut self) {
        self.lsp_send_goto("textDocument/typeDefinition", None, |buf, orig| {
            LspPendingRequest::GotoTypeDefinition {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gi` — goto implementation.
    pub(crate) fn lsp_goto_implementation(&mut self) {
        self.lsp_send_goto("textDocument/implementation", None, |buf, orig| {
            LspPendingRequest::GotoImplementation {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// `gr` — goto references (always opens picker). LSP requires a
    /// `context: { includeDeclaration }` field on top of the standard
    /// position params; servers reject the request with a deserialization
    /// error when it's missing.
    pub(crate) fn lsp_goto_references(&mut self) {
        self.lsp_send_goto(
            "textDocument/references",
            Some(json!({ "context": { "includeDeclaration": true } })),
            |buf, orig| LspPendingRequest::GotoReferences {
                buffer_id: buf,
                origin: orig,
            },
        );
    }

    /// `K` — show hover info.
    pub(crate) fn lsp_hover(&mut self) {
        if !self.active().features.hover {
            return;
        }
        // In BLAME mode `K` shows the cursor line's commit message (the same
        // markdown popup), not an LSP symbol hover — the buffer is a read-only
        // blame view.
        if self.active().editor.is_blame() {
            let (row, col) = self.active().editor.cursor();
            let win_id = self.focused_window();
            let cell = crate::app::mouse::doc_to_cell(self, win_id, row, col).unwrap_or((0, 0));
            self.show_blame_commit_hover(row, cell);
            return;
        }
        self.lsp_send_goto("textDocument/hover", None, |buf, orig| {
            LspPendingRequest::Hover {
                buffer_id: buf,
                origin: orig,
            }
        });
    }

    /// Mouse-hover variant: send `textDocument/hover` for an explicit doc
    /// position without moving the cursor. Used by the Phase 5 hover-popup
    /// timer so the user's cursor stays in place.
    pub(crate) fn lsp_hover_at_doc(&mut self, doc_row: usize, doc_col: usize) {
        if !self.active().features.hover {
            return;
        }
        if self.lsp.is_none() {
            return; // LSP not running — silently skip mouse hover
        }
        if !self.lsp_active_supports("/hoverProvider") {
            return; // server can't hover — silently skip (mouse-idle fire)
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => return,
        };
        let uri = match hjkl_lsp::uri::from_path(&path) {
            Ok(u) => u,
            Err(_) => return,
        };
        let buffer_id = slot.buffer_id as hjkl_lsp::BufferId;
        let params = serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": doc_row as u32, "character": doc_col as u32 },
        });
        let request_id = self.lsp_alloc_request_id();
        let pending = LspPendingRequest::HoverAtMouse {
            buffer_id,
            origin: (doc_row, doc_col),
        };
        self.lsp_pending.insert(request_id, pending);
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/hover", params);
        }
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
            LspPendingRequest::HoverAtMouse { buffer_id, origin } => {
                self.handle_hover_at_mouse_response(buffer_id, origin, result);
            }
            LspPendingRequest::Completion {
                buffer_id,
                anchor_row,
                anchor_col,
                auto,
            } => {
                self.handle_completion_response(buffer_id, anchor_row, anchor_col, auto, result);
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
                    self.bus.error(format!("LSP goto: {e}"));
                    return;
                }
            }
        } else {
            self.bus.error("LSP goto: non-file URI");
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
                self.bus.error(format!("LSP {kind_label}: {}", e.message));
                return;
            }
        };
        let locs = Self::parse_goto_locations(val);
        if locs.is_empty() {
            self.bus.warn(format!("no {kind_label} found"));
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
                self.bus.error(format!("LSP references: {}", e.message));
                return;
            }
        };
        let locs = Self::parse_goto_locations(val);
        if locs.is_empty() {
            self.bus.warn("no references found");
            return;
        }
        self.open_lsp_locations_picker(&locs, "references");
    }

    /// Open a picker over a set of LSP locations.
    fn open_lsp_locations_picker(&mut self, locs: &[lsp_types::Location], kind_label: &str) {
        use crate::picker_action::AppAction;

        // Strip the editor's cwd so files inside the project show as
        // relative paths (`apps/hjkl/src/main.rs`) instead of absolute
        // ones. Files outside cwd keep their full path.
        let cwd = std::env::current_dir().ok();

        // Build (label, action) pairs.
        let entries: Vec<(String, AppAction)> = locs
            .iter()
            .filter_map(|loc| {
                let url: url::Url = url::Url::parse(loc.uri.as_str()).ok()?;
                let path = hjkl_lsp::uri::to_path(&url)?;
                let row = loc.range.start.line;
                let col = loc.range.start.character as usize;
                let display_path = cwd
                    .as_ref()
                    .and_then(|c| path.strip_prefix(c).ok())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                let label = format!("{display_path}:{}: col {}", row + 1, col + 1);
                // Use OpenPathAtLine for the action — goto_line is 1-based.
                Some((label, AppAction::OpenPathAtLine(path, row + 1)))
            })
            .collect();

        if entries.is_empty() {
            self.bus.warn(format!("no {kind_label} found"));
            return;
        }

        let source = Box::new(crate::picker_sources::StaticListSource::new(
            kind_label.to_string(),
            entries,
        ));
        self.picker = Some(crate::picker::Picker::new(source));
    }

    // ── Completion ────────────────────────────────────────────────────────

    /// Locate the `completionProvider` capability object for the active
    /// buffer's language server, if one is attached. Returns the object so
    /// callers can inspect `triggerCharacters` etc.
    fn active_completion_provider(&self) -> Option<&serde_json::Value> {
        let lang = self
            .active()
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext)?;
        self.lsp_state.iter().find_map(|(key, info)| {
            (key.language == lang)
                .then(|| info.capabilities.pointer("/completionProvider"))
                .flatten()
        })
    }

    /// `true` when an LSP server is attached for the buffer in `slot_idx`
    /// (its language has an initialized entry in `lsp_state`). When a server
    /// owns diagnostics for a buffer, the noisy tree-sitter parse-error gutter
    /// signs are suppressed — a single syntax error makes the TS error-recovery
    /// flag a cascade of downstream lines, which conflicts with the server's
    /// precise diagnostics. TS signs remain a fallback for non-LSP buffers.
    pub(crate) fn slot_has_lsp(&self, slot_idx: usize) -> bool {
        let Some(slot) = self.slots.get(slot_idx) else {
            return false;
        };
        let lang = slot
            .filename
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .and_then(language_id_for_ext);
        match lang {
            Some(l) => self.lsp_state.keys().any(|k| k.language == l),
            None => false,
        }
    }

    /// `true` when a `textDocument/completion` request is already in flight.
    /// Used to suppress request storms while auto-completing as the user types.
    fn lsp_has_pending_completion(&self) -> bool {
        self.lsp_pending
            .values()
            .any(|p| matches!(p, LspPendingRequest::Completion { .. }))
    }

    /// Auto-fire completion as the user types in insert mode.
    ///
    /// Fires when `ch` is either a server-declared trigger character (e.g. `.`,
    /// `::`) **or** an identifier character (letter / digit / `_`) so the popup
    /// surfaces keywords, locals, fields and types mid-word — not only after a
    /// member-access dot.
    ///
    /// When an LSP server is attached, fires a `textDocument/completion` request
    /// (its response is augmented with buffer words). When no server is
    /// attached, opens a buffer-words-only popup synchronously so completion of
    /// already-typed identifiers still works without a language server.
    pub(crate) fn maybe_auto_trigger_completion(&mut self, ch: char) {
        // Decide whether to fire, and whether an LSP server is available, while
        // only borrowing `self` immutably — then act with the mutable borrow.
        let (has_provider, is_trigger) = match self.active_completion_provider() {
            Some(p) => {
                let is_trigger = p
                    .pointer("/triggerCharacters")
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| {
                        arr.iter()
                            .any(|s| s.as_str() == Some(ch.to_string().as_str()))
                    });
                (true, is_trigger)
            }
            None => (false, false),
        };
        let is_ident = ch.is_alphanumeric() || ch == '_';
        if !(is_trigger || is_ident) {
            return;
        }
        // Surface buffer-dictionary words immediately when typing an identifier,
        // so completion works regardless of whether a language server is
        // attached or responsive (e.g. a TOML server that never answers). When
        // an LSP response lands it replaces this popup with merged server +
        // buffer-word results.
        if is_ident {
            self.open_buffer_word_completion();
        }
        // Augment with LSP results when a server offers completion. The pending
        // guard avoids a request storm; the local prefix filter narrows the
        // open popup until the response lands.
        if has_provider && !self.lsp_has_pending_completion() {
            self.lsp_request_completion_inner(true);
        }
    }

    /// Open a completion popup populated purely from unique identifier tokens
    /// found across all open buffers (vim's keyword completion, `<C-n>`). Used
    /// when no LSP server is attached so word completion still works.
    pub(crate) fn open_buffer_word_completion(&mut self) {
        let cursor = self.active().editor.buffer().cursor();
        let (row, col) = (cursor.row, cursor.col);
        let anchor_col = self.identifier_start_col(row, col);
        let token = self.token_between(row, anchor_col, col);
        let items = self.buffer_word_items(&token);
        if items.is_empty() {
            return;
        }
        let mut popup = Completion::new(row, anchor_col, items);
        if !token.is_empty() {
            popup.set_prefix(&token);
            if popup.is_empty() {
                return;
            }
        }
        self.completion = Some(popup);
    }

    /// The text between byte columns `[lo, hi)` on `row` (the partial word
    /// under the cursor). Empty when out of range.
    fn token_between(&self, row: usize, lo: usize, hi: usize) -> String {
        let rope = self.active().editor.buffer().rope();
        if row >= rope.len_lines() {
            return String::new();
        }
        let line = hjkl_buffer::rope_line_str(&rope, row);
        let lo = lo.min(line.len());
        let hi = hi.min(line.len());
        if lo <= hi {
            line[lo..hi].to_string()
        } else {
            String::new()
        }
    }

    /// Collect unique identifier tokens from every open buffer as completion
    /// items (kind `Other`). Tokens must start with a letter or `_` and be at
    /// least two chars; `exclude` (the partial word currently under the cursor)
    /// is skipped so the popup never suggests exactly what's being typed.
    ///
    /// Bounded: scans at most `MAX_SCAN_BYTES` per buffer and returns at most
    /// `MAX_WORDS` items so a giant buffer can't stall the insert path.
    pub(crate) fn buffer_word_items(
        &self,
        exclude: &str,
    ) -> Vec<crate::completion::CompletionItem> {
        use crate::completion::CompletionItem;
        const MAX_WORDS: usize = 2000;
        const MAX_SCAN_BYTES: usize = 1_000_000;

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut items: Vec<CompletionItem> = Vec::new();

        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
        let is_word_start = |c: char| c.is_alphabetic() || c == '_';

        'buffers: for slot in &self.slots {
            let rope = slot.editor.buffer().rope();
            let mut word = String::new();
            let mut scanned = 0usize;
            for ch in rope.chars() {
                scanned += ch.len_utf8();
                if is_word_char(ch) {
                    word.push(ch);
                } else if !word.is_empty() {
                    Self::push_buffer_word(
                        &mut word,
                        exclude,
                        &mut seen,
                        &mut items,
                        is_word_start,
                    );
                    if items.len() >= MAX_WORDS {
                        break 'buffers;
                    }
                }
                if scanned >= MAX_SCAN_BYTES {
                    break;
                }
            }
            if !word.is_empty() {
                Self::push_buffer_word(&mut word, exclude, &mut seen, &mut items, is_word_start);
                if items.len() >= MAX_WORDS {
                    break 'buffers;
                }
            }
        }
        items
    }

    /// Helper for [`Self::buffer_word_items`]: accept `word` as a candidate when
    /// it is a valid identifier, not the excluded prefix, and not already seen.
    /// Clears `word` for reuse either way.
    fn push_buffer_word(
        word: &mut String,
        exclude: &str,
        seen: &mut std::collections::HashSet<String>,
        items: &mut Vec<crate::completion::CompletionItem>,
        is_word_start: impl Fn(char) -> bool,
    ) {
        if word.len() >= 2
            && word.as_str() != exclude
            && word.chars().next().is_some_and(&is_word_start)
            && seen.insert(word.clone())
        {
            items.push(crate::completion::CompletionItem::new(word.clone()));
        }
        word.clear();
    }

    /// Send a `textDocument/completion` request for the current cursor position
    /// (manual `<C-n>`/`<C-p>` invocation).
    pub(crate) fn lsp_request_completion(&mut self) {
        self.lsp_request_completion_inner(false);
    }

    /// Shared completion request. `auto` marks implicit (as-you-type) requests
    /// so an empty server response stays silent instead of flashing a warning.
    ///
    /// The popup anchor is snapped back to the **start of the identifier under
    /// the cursor** (not the raw cursor column) so the local prefix filter
    /// tracks the whole partial word as the user keeps typing. The LSP request
    /// itself still uses the true cursor position.
    fn lsp_request_completion_inner(&mut self, auto: bool) {
        if self.lsp.is_none() {
            if !auto {
                self.bus
                    .error("LSP: not enabled (set [lsp] enabled = true in config)");
            }
            return;
        }
        // Skip entirely when no initialized server advertises completion — an
        // auto-fire on every keystroke to a server that never answers would
        // otherwise pile up pending requests and hang the status spinner.
        if !self.lsp_active_supports("/completionProvider") {
            if !auto {
                self.bus.error("LSP: server has no completion support");
            }
            return;
        }
        let (params, buffer_id, (row, col)) = match self.lsp_position_params() {
            Some(v) => v,
            None => {
                if !auto {
                    self.bus.error(
                        "LSP: no file open in this buffer (use :e <file> or open from the picker)",
                    );
                }
                return;
            }
        };
        let anchor_col = self.identifier_start_col(row, col);
        let request_id = self.lsp_alloc_request_id();
        self.lsp_pending.insert(
            request_id,
            LspPendingRequest::Completion {
                buffer_id,
                anchor_row: row,
                anchor_col,
                auto,
            },
        );
        if let Some(mgr) = self.lsp.as_ref() {
            mgr.send_request(request_id, buffer_id, "textDocument/completion", params);
        }
    }

    /// Scan left from byte column `col` on `row` over identifier characters
    /// (alphanumeric / `_`) and return the byte column where the current word
    /// begins. When the character before the cursor is not an identifier char
    /// (e.g. right after a `.`), this returns `col` unchanged.
    ///
    /// Byte-based to match the cursor column (UTF-8 `positionEncoding`) and the
    /// byte-slice prefix tracking in the insert-mode key handler.
    pub(crate) fn identifier_start_col(&self, row: usize, col: usize) -> usize {
        let rope = self.active().editor.buffer().rope();
        if row >= rope.len_lines() {
            return col;
        }
        let line = hjkl_buffer::rope_line_str(&rope, row);
        // Clamp to the line length AND down to a char boundary: `col` is a byte
        // column but may land inside a multibyte char (e.g. a nerd-font icon in
        // the explorer tree), which would panic the `line[..end]` slice below.
        let mut end = col.min(line.len());
        while end > 0 && !line.is_char_boundary(end) {
            end -= 1;
        }
        let mut start = end;
        // `char_indices` is double-ended, so walk back from the cursor over the
        // contiguous run of identifier chars; `b` is the char's byte offset.
        for (b, c) in line[..end].char_indices().rev() {
            if c.is_alphanumeric() || c == '_' {
                start = b;
            } else {
                break;
            }
        }
        start
    }

    // ── Code actions ──────────────────────────────────────────────────────

    /// `<leader>ca` — request code actions at the cursor.
    pub(crate) fn lsp_code_actions(&mut self) {
        if self.lsp.is_none() {
            self.bus
                .error("LSP: not enabled (set [lsp] enabled = true in config)");
            return;
        }
        if !self.lsp_active_supports("/codeActionProvider") {
            self.bus.error("LSP: server has no code-action support");
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.bus.error(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)",
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.bus.error("LSP: cannot build URI");
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
                self.bus.error(format!("LSP codeAction: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.bus.warn("no code actions");
            return;
        }

        let actions: Vec<lsp_types::CodeActionOrCommand> = match serde_json::from_value(val) {
            Ok(a) => a,
            Err(_) => {
                self.bus.error("LSP codeAction: could not parse response");
                return;
            }
        };

        if actions.is_empty() {
            self.bus.warn("no code actions");
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
                            self.bus.info(format!("{count} files changed"));
                        }
                        Err(e) => {
                            self.bus.error(format!("LSP codeAction: {e}"));
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
            self.bus
                .error("LSP: not enabled (set [lsp] enabled = true in config)");
            return;
        }
        if !self.lsp_active_supports("/renameProvider") {
            self.bus.error("LSP: server has no rename support");
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.bus.error(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)",
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.bus.error("LSP: cannot build URI");
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
                self.bus.error(format!("LSP rename: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.bus.error("E: cannot rename here");
            return;
        }

        let workspace_edit: lsp_types::WorkspaceEdit = match serde_json::from_value(val) {
            Ok(we) => we,
            Err(_) => {
                self.bus.error("LSP rename: could not parse response");
                return;
            }
        };

        match self.apply_workspace_edit(workspace_edit) {
            Ok(count) => {
                self.bus.info(format!("renamed: {count} files changed"));
            }
            Err(e) => {
                self.bus.error(format!("LSP rename: {e}"));
            }
        }
    }

    // ── Format ────────────────────────────────────────────────────────────

    /// `:LspFormat` — send a `textDocument/formatting` request.
    pub(crate) fn lsp_format(&mut self) {
        if self.lsp.is_none() {
            self.bus
                .error("LSP: not enabled (set [lsp] enabled = true in config)");
            return;
        }
        if !self.lsp_active_supports("/documentFormattingProvider") {
            self.bus.error("LSP: server has no formatting support");
            return;
        }
        let slot = self.active();
        let path = match slot.filename.as_ref() {
            Some(p) => absolutize(p),
            None => {
                self.bus.error(
                    "LSP: no file open in this buffer (use :e <file> or open from the picker)",
                );
                return;
            }
        };
        let uri = match hjkl_lsp::uri::from_path(&path).ok() {
            Some(u) => u,
            None => {
                self.bus.error("LSP: cannot build URI");
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
                self.bus.error(format!("LSP format: {}", e.message));
                return;
            }
        };

        if val.is_null() {
            self.bus.warn("no formatting changes");
            return;
        }

        let edits: Vec<lsp_types::TextEdit> = match serde_json::from_value(val) {
            Ok(e) => e,
            Err(_) => {
                self.bus.error("LSP format: could not parse response");
                return;
            }
        };

        if edits.is_empty() {
            self.bus.warn("no formatting changes");
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
                self.bus.error("LSP format: buffer no longer open");
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
        self.bus.info("formatted");
    }

    /// Handle a `textDocument/completion` response.
    pub(crate) fn handle_completion_response(
        &mut self,
        buffer_id: hjkl_lsp::BufferId,
        anchor_row: usize,
        anchor_col: usize,
        auto: bool,
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.bus.error(format!("LSP completion: {}", e.message));
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

        // The partial word currently under the cursor — used both to filter the
        // popup and as the `exclude` token for buffer-word harvesting.
        let cursor = self.active().editor.buffer().cursor();
        let prefix = if cursor.row == anchor_row && cursor.col >= anchor_col {
            self.token_between(anchor_row, anchor_col, cursor.col)
        } else {
            String::new()
        };

        let mut items: Vec<crate::completion::CompletionItem> =
            lsp_items.into_iter().map(item_from_lsp).collect();

        // Augment LSP results with unique tokens from the open buffers (vim-like
        // keyword completion). Dedupe by label so a word the server already
        // returned isn't listed twice; buffer words rank below exact LSP hits
        // via the shared fuzzy scorer once the prefix is applied.
        let existing: std::collections::HashSet<String> =
            items.iter().map(|i| i.label.clone()).collect();
        for w in self.buffer_word_items(&prefix) {
            if !existing.contains(&w.label) {
                items.push(w);
            }
        }

        if items.is_empty() {
            // As-you-type requests stay quiet; only an explicit invocation
            // reports an empty result.
            if !auto {
                self.bus.warn("no completions");
            }
            return;
        }

        let mut popup = Completion::new(anchor_row, anchor_col, items);
        // The user may have typed further chars between firing the request and
        // this response landing. Filter the freshly-opened popup by whatever is
        // currently between the anchor and the cursor so it shows the right
        // subset immediately rather than the full list for one frame.
        if !prefix.is_empty() {
            popup.set_prefix(&prefix);
        }
        if popup.is_empty() {
            return;
        }
        self.completion = Some(popup);
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

        let cursor = self.active().editor.buffer().cursor();
        let row = cursor.row;
        let cur_col = cursor.col;
        let anchor_col = popup.anchor_col.min(cur_col);

        // For Function/Method items: ensure parens exist and place cursor inside.
        // Strip trivial snippet markers ($0, ${0}) before examining the text.
        let raw_text = &item.insert_text;
        let is_fn = matches!(
            item.kind,
            crate::completion::CompletionKind::Function | crate::completion::CompletionKind::Method
        );

        let (actual_text, cursor_offset) = if is_fn {
            // Strip a trailing `$0` or `${0}` snippet placeholder if present.
            let stripped = if raw_text.ends_with("$0") {
                &raw_text[..raw_text.len() - 2]
            } else if raw_text.ends_with("${0}") {
                &raw_text[..raw_text.len() - 4]
            } else {
                raw_text.as_str()
            };

            if let Some(paren_pos) = stripped.find('(') {
                // Already has parens — place cursor after the `(`.
                (stripped.to_string(), paren_pos + 1)
            } else {
                // Bare name — append `()` and place cursor between them.
                let with_parens = format!("{}()", stripped);
                let offset = stripped.len() + 1; // position after `(`
                (with_parens, offset)
            }
        } else {
            // Non-function: cursor at end of inserted text.
            (raw_text.clone(), raw_text.len())
        };

        // Replace [anchor_col, cur_col) with actual_text via the editor's
        // tracked mutation funnel. Going through `mutate_edit` (rather than
        // poking `buffer_mut()` directly) records a `ContentEdit` so the host's
        // `sync_after_engine_mutation` drains it → `syntax.apply_edits` updates
        // the tree-sitter tree and `lsp_notify_change_active` fires a didChange.
        // Without this, accepting a completion left the parse tree and the LSP
        // server with stale text — diagnostics / parse-error gutter signs for
        // the now-fixed line lingered until the next manual edit (#143).
        use hjkl_buffer::{Edit, Position};
        self.active_mut().editor.mutate_edit(Edit::Replace {
            start: Position::new(row, anchor_col),
            end: Position::new(row, cur_col),
            with: actual_text,
        });

        // Move cursor to computed offset within the inserted text.
        let new_col = anchor_col + cursor_offset;
        self.active_mut().editor.jump_cursor(row, new_col);
        // completion was already taken via `take()` above, so it's already None.
    }

    /// Handle a hover response (K-key) — show the compact cursor-anchored hover
    /// popup, the same widget mouse hover uses (content-sized, not an 80%×60%
    /// modal). `origin` is the cursor's doc `(row, col)` from the request.
    pub(crate) fn handle_hover_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        origin: (usize, usize),
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        let val = match result {
            Ok(v) => v,
            Err(e) => {
                self.bus.error(format!("LSP hover: {}", e.message));
                return;
            }
        };
        if val.is_null() {
            self.bus.warn("no hover info");
            return;
        }
        let hover: lsp_types::Hover = match serde_json::from_value(val) {
            Ok(h) => h,
            Err(_) => {
                self.bus.error("LSP hover: could not parse response");
                return;
            }
        };
        let text = extract_hover_markdown(&hover.contents);
        if text.trim().is_empty() {
            self.bus.warn("no hover info");
        } else {
            // Anchor at the cursor cell and reuse the mouse-hover popup so K
            // and mouse hover share the same compact, content-sized rendering.
            let win_id = self.focused_window();
            let (doc_row, doc_col) = origin;
            let cell =
                crate::app::mouse::doc_to_cell(self, win_id, doc_row, doc_col).unwrap_or((0, 0));
            self.hover_popup = Some(crate::hover_popup::new(text, cell));
        }
    }

    /// Handle a mouse-hover response — set `hover_popup` if the timer is
    /// still armed at the same cell. Drops the result if the user moved.
    pub(crate) fn handle_hover_at_mouse_response(
        &mut self,
        _buffer_id: hjkl_lsp::BufferId,
        _origin: (usize, usize),
        result: Result<serde_json::Value, hjkl_lsp::RpcError>,
    ) {
        // Drop the response if a blocking overlay (context menu, picker,
        // command field) opened while the RPC was in flight — the popup
        // would render through the overlay onto the buffer text behind it.
        if self.overlay_active() {
            return;
        }
        // Only show the popup when the timer is still armed and the RPC was
        // sent from it (guard against stale in-flight requests).
        let timer_cell = match &self.hover_timer {
            Some(t) if t.request_sent => t.cell,
            _ => return, // user moved before the response arrived
        };

        let val = match result {
            Ok(v) => v,
            Err(_) => return, // silently drop errors for mouse hover
        };
        if val.is_null() {
            return; // no hover info — don't show an empty popup
        }
        let hover: lsp_types::Hover = match serde_json::from_value(val) {
            Ok(h) => h,
            Err(_) => return,
        };
        let text = extract_hover_markdown(&hover.contents);
        if text.trim().is_empty() {
            return;
        }
        self.hover_popup = Some(crate::hover_popup::new(text, timer_cell));
    }
}

/// Extract raw markdown from LSP hover contents for rendering by
/// `hjkl-markdown-tui` (K-key popup and mouse hover popup paths).
///
/// `MarkedString::LanguageString` is wrapped in a CommonMark fenced block so
/// the markdown renderer can syntax-highlight it.
fn extract_hover_markdown(contents: &lsp_types::HoverContents) -> String {
    match contents {
        lsp_types::HoverContents::Scalar(ms) => marked_string_to_md(ms),
        lsp_types::HoverContents::Array(items) => items
            .iter()
            .map(marked_string_to_md)
            .collect::<Vec<_>>()
            .join("\n\n"),
        // MarkupContent: already markdown (or plain text for `kind = "plaintext"`).
        lsp_types::HoverContents::Markup(mc) => mc.value.clone(),
    }
}

fn marked_string_to_md(ms: &lsp_types::MarkedString) -> String {
    match ms {
        // Plain string — pass through as-is; valid CommonMark.
        lsp_types::MarkedString::String(s) => s.clone(),
        // Language-annotated snippet — wrap in a fenced code block.
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

impl App {
    /// Dispatch an LSP-related [`crate::keymap_actions::AppAction`].
    ///
    /// Handles variants:
    ///   - ShowDiagAtCursor, LspCodeActions, LspRename
    ///   - LspGotoDef, LspGotoDecl, LspGotoRef, LspGotoImpl, LspGotoTypeDef
    ///   - LspHover
    ///   - DiagNext, DiagPrev, DiagNextError, DiagPrevError
    pub(crate) fn dispatch_lsp_action(&mut self, action: crate::keymap_actions::AppAction) {
        use crate::keymap_actions::AppAction;
        match action {
            AppAction::ShowDiagAtCursor => self.show_diag_at_cursor(),
            AppAction::LspCodeActions => self.lsp_code_actions(),
            AppAction::LspRename => {
                // Phase 5 MVP: prompt user to use :Rename <newname>.
                self.bus.info("use :Rename <newname> to rename");
            }
            AppAction::LspGotoDef => self.lsp_goto_definition(),
            AppAction::LspGotoDecl => self.lsp_goto_declaration(),
            AppAction::LspGotoRef => self.lsp_goto_references(),
            AppAction::LspGotoImpl => self.lsp_goto_implementation(),
            AppAction::LspGotoTypeDef => self.lsp_goto_type_definition(),
            AppAction::LspHover => self.lsp_hover(),
            AppAction::DiagNext => self.dispatch_ex("lnext"),
            AppAction::DiagPrev => self.dispatch_ex("lprev"),
            AppAction::DiagNextError => self.lnext_severity(Some(super::DiagSeverity::Error)),
            AppAction::DiagPrevError => self.lprev_severity(Some(super::DiagSeverity::Error)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod lsp_glue_tests {
    use super::snap_to_char_boundary;

    /// `snap_to_char_boundary` must floor a byte index that falls inside a
    /// multi-byte char (here a 4-byte emoji U+1F600) to the char's first byte.
    #[test]
    fn snap_floors_interior_emoji_byte() {
        // "😀" encodes as 4 bytes: F0 9F 98 80
        let rope = ropey::Rope::from_str("hi😀bye");
        // Char starts at byte 2; bytes 3, 4, 5 are interior continuation bytes.
        let emoji_start = 2usize;
        for interior in emoji_start..emoji_start + 4 {
            let snapped = snap_to_char_boundary(&rope, interior);
            assert_eq!(
                snapped, emoji_start,
                "byte {interior} inside emoji should snap to byte {emoji_start}, got {snapped}"
            );
        }
        // Byte past the emoji should snap to itself (it's already aligned).
        let past = emoji_start + 4;
        assert_eq!(snap_to_char_boundary(&rope, past), past);
    }

    /// `build_text_changes` must not panic when `new_end_byte` is clamped
    /// into the middle of a multi-byte char.
    #[test]
    fn build_text_changes_no_panic_on_emoji() {
        use hjkl_engine::ContentEdit;
        // Build a rope whose length falls mid-emoji when clamped.
        // "abc😀" = 3 + 4 = 7 bytes; len_bytes = 7.
        // Craft an edit whose new_end_byte = len - 2 lands inside the 4-byte emoji.
        let rope = ropey::Rope::from_str("abc😀\n");
        let len = rope.len_bytes();
        let edit = ContentEdit {
            start_byte: 0,
            old_end_byte: 0,
            new_end_byte: len.saturating_sub(2), // interior emoji byte
            start_position: (0, 0),
            old_end_position: (0, 0),
            new_end_position: (0, 0),
        };
        // Must not panic.
        let changes = super::build_text_changes(&rope, &[edit]);
        assert!(!changes.is_empty());
        // The extracted text must be valid UTF-8 (implicit: it compiled to String).
        assert!(std::str::from_utf8(changes[0].text.as_bytes()).is_ok());
    }
}
