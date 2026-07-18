//! Cross-thread message types for the LSP subsystem.

use std::path::PathBuf;
use std::sync::Arc;

use crate::BufferId;

/// One element of a `textDocument/didChange` `contentChanges` array when the
/// client is using incremental sync. `start_*` / `end_*` are positions in
/// the document state immediately *before* this change is applied (and after
/// every earlier change in the same array has been applied). Position units
/// match the negotiated `positionEncoding` for the server — UTF-8 byte
/// offsets when negotiated, UTF-16 code units otherwise.
#[derive(Debug, Clone)]
pub struct TextChange {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    /// Replacement text inserted at `[start, end)`.
    pub text: String,
}

/// Unique key identifying an LSP server instance: one per (language, workspace root).
#[derive(Clone, Eq, Hash, PartialEq, Debug)]
pub struct ServerKey {
    pub language: String,
    pub root: PathBuf,
}

/// Commands sent from the app (sync) side to the async LSP runtime.
#[derive(Debug)]
pub enum LspCommand {
    /// Open a buffer in the LSP server for `language_id`.
    AttachBuffer {
        id: BufferId,
        path: PathBuf,
        language_id: String,
        text: String,
    },
    /// Remove a buffer from the LSP server.
    DetachBuffer { id: BufferId },
    /// Notify the server that a buffer's full text changed (full-doc sync).
    /// Used when the server announced Full sync or when the client cannot
    /// build incremental changes (e.g. server uses an unsupported
    /// `positionEncoding`).
    NotifyChange {
        id: BufferId,
        full_text: Arc<String>,
    },
    /// Notify the server of an incremental edit batch. `changes` are applied
    /// in order; each position is interpreted relative to the document state
    /// after every preceding change has been applied. No-op when `changes`
    /// is empty.
    NotifyChangeIncremental {
        id: BufferId,
        changes: Vec<TextChange>,
    },
    /// Notify the server that a buffer was saved (`textDocument/didSave`).
    /// Triggers the server's on-save flycheck (e.g. rust-analyzer clippy).
    NotifySave { id: BufferId },
    /// Cancel an in-flight request by id. Reserved for Phase 4.
    Cancel { request_id: i64 },
    /// Send a JSON-RPC request to the server attached to `buffer_id`.
    /// `request_id` is app-allocated (monotonic counter); it will appear in
    /// `LspEvent::Response { request_id, .. }` so the app can correlate
    /// the response without knowing the server's internal numbering.
    Request {
        request_id: i64,
        buffer_id: crate::BufferId,
        method: String,
        params: serde_json::Value,
    },
    /// A server process exited; remove runtime state so a future attachment
    /// starts a replacement.
    ServerExited { key: ServerKey },
    /// Gracefully shut down all servers and exit the runtime loop.
    ShutdownAll,
}

/// A JSON-RPC error payload from a server response.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// Events emitted from the async LSP runtime to the app (sync) side.
#[derive(Debug)]
pub enum LspEvent {
    /// An LSP server finished its `initialize` handshake.
    ServerInitialized {
        key: ServerKey,
        capabilities: serde_json::Value,
    },
    /// An LSP server process exited.
    ServerExited {
        key: ServerKey,
        status: std::process::ExitStatus,
    },
    /// A push notification arrived from a server (e.g. `textDocument/publishDiagnostics`).
    Notification {
        key: ServerKey,
        method: String,
        params: serde_json::Value,
    },
    /// A response to a request we sent arrived.
    Response {
        request_id: i64,
        result: Result<serde_json::Value, RpcError>,
    },
}
