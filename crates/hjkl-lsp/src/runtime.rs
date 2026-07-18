//! Async dispatch loop: owns server actors and buffer attachment state.

use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use url::Url;

use std::sync::Arc;

use crate::BufferId;
use crate::config::LspConfig;
use crate::event::{LspCommand, LspEvent, ServerKey, TextChange};
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
                handle_detach(id, &mut servers, &mut buffers).await;
            }
            LspCommand::NotifyChange { id, full_text } => {
                handle_notify_change(id, full_text, &mut servers, &mut buffers);
            }
            LspCommand::NotifyChangeIncremental { id, changes } => {
                handle_notify_change_incremental(id, changes, &mut servers, &mut buffers);
            }
            LspCommand::NotifySave { id } => {
                handle_notify_save(id, &mut servers, &mut buffers);
            }
            LspCommand::Cancel { request_id } => {
                // Phase 4 will cancel in-flight requests. For now just log.
                tracing::debug!(
                    request_id,
                    "LspCommand::Cancel received (Phase 4 placeholder)"
                );
            }
            LspCommand::Request {
                request_id,
                buffer_id,
                method,
                params,
            } => {
                handle_request(
                    request_id,
                    buffer_id,
                    &method,
                    params,
                    &mut servers,
                    &buffers,
                );
            }
            LspCommand::ServerExited { key } => {
                servers.remove(&key);
                buffers.retain(|_, buffer| buffer.server_key != key);
                tracing::info!(?key, "removed exited LSP server and its buffer attachments");
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

async fn handle_detach(
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

    // Reference-count attached buffers per server, derived from the existing
    // `buffers` map rather than a parallel counter: if no remaining buffer
    // references this server's key, nothing needs it anymore. Shut it down
    // (reusing the same graceful-shutdown path `ShutdownAll` uses per server)
    // and drop it from `servers` so a later attach for this key re-spawns a
    // fresh server instead of finding a stale entry.
    let still_in_use = buffers.values().any(|b| b.server_key == buf.server_key);
    if !still_in_use && let Some(server) = servers.remove(&buf.server_key) {
        tracing::info!(
            key = ?buf.server_key,
            "last buffer detached; shutting down LSP server"
        );
        server.shutdown().await;
    }
}

fn handle_request(
    request_id: i64,
    buffer_id: BufferId,
    method: &str,
    params: serde_json::Value,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &HashMap<BufferId, AttachedBuffer>,
) {
    let buf = match buffers.get(&buffer_id) {
        Some(b) => b,
        None => {
            tracing::debug!(buffer_id, method, "Request: buffer not attached");
            return;
        }
    };
    if let Some(server) = servers.get_mut(&buf.server_key) {
        server.send_request(request_id, method, params);
    } else {
        tracing::debug!(key = ?buf.server_key, method, "Request: server not found");
    }
}

fn handle_notify_change(
    id: BufferId,
    full_text: Arc<String>,
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
                "contentChanges": [{ "text": full_text.as_str() }],
            }),
        );
    }
}

fn handle_notify_save(
    id: BufferId,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &mut HashMap<BufferId, AttachedBuffer>,
) {
    let buf = match buffers.get(&id) {
        Some(b) => b,
        None => {
            tracing::debug!(id, "NotifySave: buffer not attached");
            return;
        }
    };
    let uri = buf.uri.as_str().to_string();
    if let Some(server) = servers.get_mut(&buf.server_key) {
        server.send_notification(
            "textDocument/didSave",
            json!({ "textDocument": { "uri": uri } }),
        );
    }
}

fn handle_notify_change_incremental(
    id: BufferId,
    changes: Vec<TextChange>,
    servers: &mut HashMap<ServerKey, Server>,
    buffers: &mut HashMap<BufferId, AttachedBuffer>,
) {
    if changes.is_empty() {
        return;
    }
    let buf = match buffers.get_mut(&id) {
        Some(b) => b,
        None => {
            tracing::debug!(id, "NotifyChangeIncremental: buffer not attached");
            return;
        }
    };
    buf.version += 1;
    let version = buf.version;
    let uri = buf.uri.as_str().to_string();

    if let Some(server) = servers.get_mut(&buf.server_key) {
        let content_changes: Vec<serde_json::Value> = changes
            .into_iter()
            .map(|c| {
                json!({
                    "range": {
                        "start": { "line": c.start_line, "character": c.start_col },
                        "end":   { "line": c.end_line,   "character": c.end_col },
                    },
                    "text": c.text,
                })
            })
            .collect();
        server.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": content_changes,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    //! `handle_detach` reference-counting tests. These drive `Server`
    //! instances backed by an in-memory `duplex` pair (no real child
    //! process — see `crate::server::spawn_from_io`), so they exercise the
    //! exact production code path (including the graceful `Server::shutdown`
    //! sequence `ShutdownAll` also uses) without spawning anything.

    use std::path::PathBuf;
    use std::time::Duration;

    use tokio::io::{AsyncWrite, BufReader, DuplexStream, ReadHalf, WriteHalf, duplex};

    use super::*;
    use crate::server;

    async fn read_json<R: tokio::io::AsyncRead + Unpin>(r: &mut BufReader<R>) -> serde_json::Value {
        let bytes = crate::codec::read_message(r)
            .await
            .expect("read_json: io error")
            .expect("read_json: clean EOF before message");
        serde_json::from_slice(&bytes).expect("read_json: invalid JSON")
    }

    async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, val: &serde_json::Value) {
        let bytes = serde_json::to_vec(val).unwrap();
        crate::codec::write_message(w, &bytes).await.unwrap();
    }

    /// Absolute path under a tmp-style prefix accepted by
    /// `url::Url::from_file_path` on every platform (mirrors the helper in
    /// `tests/mock_server.rs`).
    fn workspace_root(leaf: &str) -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from(format!("/tmp/{leaf}"))
        }
        #[cfg(windows)]
        {
            PathBuf::from(format!(r"C:\{leaf}"))
        }
    }

    /// Spawn a mock `Server` over an in-memory duplex pair, driving the
    /// `initialize` handshake to completion. Returns the ready `Server`
    /// plus the driver's read/write halves so the test can observe further
    /// protocol traffic (didClose / shutdown / exit).
    async fn mock_server(
        key: ServerKey,
    ) -> (
        Server,
        BufReader<ReadHalf<DuplexStream>>,
        WriteHalf<DuplexStream>,
    ) {
        let (client_io, driver_io) = duplex(64 * 1024);
        let (evt_tx, _evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        let server_task = tokio::spawn({
            let key = key.clone();
            async move { server::spawn_from_io(key, client_write, client_read, evt_tx).await }
        });

        let req = read_json(&mut driver_reader).await;
        let req_id = req["id"].as_i64().unwrap();
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": { "capabilities": {} }
            }),
        )
        .await;
        let _initialized = read_json(&mut driver_reader).await; // "initialized" notification

        let server = server_task
            .await
            .expect("mock_server task panicked")
            .expect("spawn_from_io failed");
        (server, driver_reader, driver_write)
    }

    /// Build an `AttachedBuffer` pointing at `key` for a fake file under
    /// `root`. The exact path only needs to convert cleanly to a `file://`
    /// URI — its contents don't matter to `handle_detach`.
    fn attached_buffer(root: &std::path::Path, leaf: &str, key: ServerKey) -> AttachedBuffer {
        AttachedBuffer {
            uri: crate::uri::from_path(&root.join(leaf)).unwrap(),
            server_key: key,
            version: 1,
        }
    }

    /// Detaching one of *two* buffers attached to the same server must leave
    /// the server running: the other buffer still needs it. Only a
    /// `textDocument/didClose` should go out — no shutdown/exit.
    #[tokio::test(flavor = "current_thread")]
    async fn detach_keeps_server_alive_with_remaining_buffers() {
        tokio::time::timeout(Duration::from_millis(500), async {
            let root = workspace_root("refcount-keep-alive");
            let key = ServerKey {
                language: "rust".to_string(),
                root: root.clone(),
            };
            let (server, mut driver_reader, driver_write) = mock_server(key.clone()).await;

            let mut servers = HashMap::new();
            servers.insert(key.clone(), server);

            let mut buffers = HashMap::new();
            buffers.insert(1, attached_buffer(&root, "a.rs", key.clone()));
            buffers.insert(2, attached_buffer(&root, "b.rs", key.clone()));

            handle_detach(1, &mut servers, &mut buffers).await;

            assert!(
                servers.contains_key(&key),
                "server shut down while buffer 2 still references it"
            );
            assert!(!buffers.contains_key(&1));
            assert!(buffers.contains_key(&2));

            let did_close = read_json(&mut driver_reader).await;
            assert_eq!(did_close["method"], "textDocument/didClose");

            // Nothing else should be queued — specifically no "shutdown".
            let probe =
                tokio::time::timeout(Duration::from_millis(50), read_json(&mut driver_reader))
                    .await;
            assert!(
                probe.is_err(),
                "unexpected extra message after didClose: server must not have been shut down"
            );

            drop(driver_write);
        })
        .await
        .expect("detach_keeps_server_alive_with_remaining_buffers timed out");
    }

    /// Detaching the LAST buffer on a server must gracefully shut it down
    /// (the same shutdown/exit sequence `ShutdownAll` uses per server) and
    /// remove it from the `servers` map — no leaked process, no tombstone
    /// entry left behind that could block a later re-attach.
    #[tokio::test(flavor = "current_thread")]
    async fn detach_last_buffer_shuts_down_and_removes_server() {
        tokio::time::timeout(Duration::from_millis(500), async {
            let root = workspace_root("refcount-last-detach");
            let key = ServerKey {
                language: "rust".to_string(),
                root: root.clone(),
            };
            let (server, mut driver_reader, driver_write) = mock_server(key.clone()).await;

            let mut servers = HashMap::new();
            servers.insert(key.clone(), server);

            let mut buffers = HashMap::new();
            buffers.insert(1, attached_buffer(&root, "a.rs", key.clone()));

            handle_detach(1, &mut servers, &mut buffers).await;

            // The core assertion: no tombstone. `handle_attach`'s only spawn
            // gate is `!servers.contains_key(&key)`, so this is exactly the
            // condition that lets a later attach for this key re-spawn a
            // fresh server instead of silently no-op'ing.
            assert!(
                !servers.contains_key(&key),
                "server must be removed from the map after its last buffer detaches"
            );
            assert!(!buffers.contains_key(&1));

            let did_close = read_json(&mut driver_reader).await;
            assert_eq!(did_close["method"], "textDocument/didClose");
            let shutdown_req = read_json(&mut driver_reader).await;
            assert_eq!(shutdown_req["method"], "shutdown");
            let exit_notif = read_json(&mut driver_reader).await;
            assert_eq!(exit_notif["method"], "exit");

            drop(driver_write);
        })
        .await
        .expect("detach_last_buffer_shuts_down_and_removes_server timed out");
    }

    /// Detaching a buffer from server A must not touch server B: different
    /// `ServerKey`s are independent reference-counting domains.
    #[tokio::test(flavor = "current_thread")]
    async fn detach_does_not_affect_other_server() {
        tokio::time::timeout(Duration::from_millis(500), async {
            let root_a = workspace_root("refcount-isolation-a");
            let root_b = workspace_root("refcount-isolation-b");
            let key_a = ServerKey {
                language: "rust".to_string(),
                root: root_a.clone(),
            };
            let key_b = ServerKey {
                language: "go".to_string(),
                root: root_b.clone(),
            };

            let (server_a, mut driver_reader_a, driver_write_a) = mock_server(key_a.clone()).await;
            let (server_b, mut driver_reader_b, driver_write_b) = mock_server(key_b.clone()).await;

            let mut servers = HashMap::new();
            servers.insert(key_a.clone(), server_a);
            servers.insert(key_b.clone(), server_b);

            let mut buffers = HashMap::new();
            buffers.insert(1, attached_buffer(&root_a, "a.rs", key_a.clone()));
            buffers.insert(2, attached_buffer(&root_b, "b.go", key_b.clone()));

            handle_detach(1, &mut servers, &mut buffers).await;

            assert!(!servers.contains_key(&key_a), "server A must be reaped");
            assert!(servers.contains_key(&key_b), "server B must be untouched");
            assert!(
                buffers.contains_key(&2),
                "server B's buffer must remain attached"
            );

            // Server A: didClose, shutdown, exit.
            let did_close = read_json(&mut driver_reader_a).await;
            assert_eq!(did_close["method"], "textDocument/didClose");
            let shutdown_req = read_json(&mut driver_reader_a).await;
            assert_eq!(shutdown_req["method"], "shutdown");
            let exit_notif = read_json(&mut driver_reader_a).await;
            assert_eq!(exit_notif["method"], "exit");

            // Server B: nothing at all.
            let probe =
                tokio::time::timeout(Duration::from_millis(50), read_json(&mut driver_reader_b))
                    .await;
            assert!(
                probe.is_err(),
                "server B must not receive any protocol traffic"
            );

            drop(driver_write_a);
            drop(driver_write_b);
        })
        .await
        .expect("detach_does_not_affect_other_server timed out");
    }
}
