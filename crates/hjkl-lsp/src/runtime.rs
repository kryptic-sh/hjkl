//! Async dispatch loop: owns server actors and buffer attachment state.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use url::Url;

use crate::BufferId;
use crate::config::LspConfig;
use crate::event::{LspCommand, LspEvent, ServerKey};
use crate::server::Server;
use crate::workspace;

/// Per-buffer attachment record.
struct AttachedBuffer {
    uri: Url,
    server_key: ServerKey,
    version: i32,
}

/// Main async dispatch loop. Runs inside `runtime.block_on(...)` on the
/// dedicated "hjkl-lsp" std::thread.
pub(crate) async fn dispatch(
    mut cmd_rx: UnboundedReceiver<LspCommand>,
    evt_tx: Sender<LspEvent>,
    config: LspConfig,
) {
    let mut servers: HashMap<ServerKey, Server> = HashMap::new();
    let mut buffers: HashMap<BufferId, AttachedBuffer> = HashMap::new();

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            LspCommand::AttachBuffer {
                id,
                path,
                language_id,
                text,
            } => {
                handle_attach(
                    id,
                    path,
                    language_id,
                    text,
                    &config,
                    &mut servers,
                    &mut buffers,
                    &evt_tx,
                )
                .await;
            }
            LspCommand::DetachBuffer { id } => {
                handle_detach(id, &mut servers, &mut buffers);
            }
            LspCommand::NotifyChange { id, full_text } => {
                handle_notify_change(id, full_text, &mut servers, &mut buffers);
            }
            LspCommand::Cancel { request_id } => {
                // Phase 4 will cancel in-flight requests. For now just log.
                tracing::debug!(
                    request_id,
                    "LspCommand::Cancel received (Phase 4 placeholder)"
                );
            }
            LspCommand::ShutdownAll => {
                tracing::info!("shutting down all LSP servers");
                for (_key, server) in servers.drain() {
                    server.shutdown().await;
                }
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_attach(
    id: BufferId,
    path: PathBuf,
    language_id: String,
    text: String,
    config: &LspConfig,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &mut HashMap<BufferId, AttachedBuffer>,
    evt_tx: &Sender<LspEvent>,
) {
    if buffers.contains_key(&id) {
        tracing::debug!(id, "AttachBuffer: already attached, ignoring");
        return;
    }

    // Look up server config for this language.
    let server_cfg = match config.servers.get(&language_id) {
        Some(c) => c,
        None => {
            tracing::debug!(
                language_id,
                "AttachBuffer: no server configured for language"
            );
            return;
        }
    };

    // Resolve workspace root.
    let markers: Vec<&str> = server_cfg.root_markers.iter().map(String::as_str).collect();
    let root = workspace::find_root(&path, &markers)
        .unwrap_or_else(|| path.parent().unwrap_or(&path).to_path_buf());

    let key = ServerKey {
        language: language_id.clone(),
        root,
    };

    // Build file URI.
    let uri = match crate::uri::from_path(&path) {
        Ok(u) => u,
        Err(_) => {
            tracing::warn!(path = ?path, "AttachBuffer: cannot convert path to URI");
            return;
        }
    };

    // Ensure server is running.
    if !servers.contains_key(&key) {
        match Server::spawn(key.clone(), server_cfg, evt_tx.clone()).await {
            Ok(server) => {
                servers.insert(key.clone(), server);
            }
            Err(e) => {
                tracing::warn!(key = ?key, "failed to spawn LSP server: {e:#}");
                return;
            }
        }
    }

    let server = servers.get_mut(&key).expect("just inserted");

    // Send textDocument/didOpen.
    server.send_notification(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri.as_str(),
                "languageId": language_id,
                "version": 1,
                "text": text,
            }
        }),
    );

    buffers.insert(
        id,
        AttachedBuffer {
            uri,
            server_key: key,
            version: 1,
        },
    );
}

fn handle_detach(
    id: BufferId,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &mut HashMap<BufferId, AttachedBuffer>,
) {
    let buf = match buffers.remove(&id) {
        Some(b) => b,
        None => {
            tracing::debug!(id, "DetachBuffer: buffer not attached");
            return;
        }
    };

    if let Some(server) = servers.get_mut(&buf.server_key) {
        server.send_notification(
            "textDocument/didClose",
            json!({
                "textDocument": { "uri": buf.uri.as_str() }
            }),
        );
    }
    // Phase 1: keep server alive — don't shut it down on detach.
}

fn handle_notify_change(
    id: BufferId,
    full_text: String,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &mut HashMap<BufferId, AttachedBuffer>,
) {
    let buf = match buffers.get_mut(&id) {
        Some(b) => b,
        None => {
            tracing::debug!(id, "NotifyChange: buffer not attached");
            return;
        }
    };
    buf.version += 1;
    let version = buf.version;
    let uri = buf.uri.as_str().to_string();

    if let Some(server) = servers.get_mut(&buf.server_key) {
        server.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{ "text": full_text }],
            }),
        );
    }
}
