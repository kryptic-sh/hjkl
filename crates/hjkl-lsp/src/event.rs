//! Cross-thread message types for the LSP subsystem.

use std::path::PathBuf;

use crate::BufferId;

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
    /// Notify the server that a buffer's full text changed.
    NotifyChange { id: BufferId, full_text: String },
    /// Cancel an in-flight request by id. Reserved for Phase 4.
    Cancel { request_id: i64 },
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
