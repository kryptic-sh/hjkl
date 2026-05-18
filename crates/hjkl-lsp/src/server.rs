//! LSP server actor — owns the child process and JSON-RPC I/O tasks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use crossbeam_channel::Sender;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

use crate::codec;
use crate::config::ServerConfig;
use crate::event::{LspEvent, RpcError, ServerKey};

/// Shared map from JSON-RPC id → app-allocated request id.
/// Inserted by `Server::send_request`; consumed by the stdout dispatch task
/// when the corresponding response arrives.
type PendingMap = Arc<Mutex<HashMap<i64, i64>>>;

/// Wraps an active language-server child process.
pub struct Server {
    pub key: ServerKey,
    pub capabilities: Value,
    /// Channel for sending serialized JSON frames to the stdin writer task.
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    next_request_id: i64,
    /// Maps JSON-RPC request id → app-allocated request id.
    /// Shared with the stdout dispatch task so responses can be correlated.
    pending: PendingMap,
}

impl Server {
    /// Spawn a language-server process, perform the `initialize` handshake,
    /// and return a ready `Server`.
    ///
    /// Three background tasks are spawned:
    /// - **stdin task** — drains `stdin_tx`, writes framed messages.
    /// - **stdout task** — reads framed messages, dispatches responses/notifications.
    /// - **stderr task** — reads log lines, emits `tracing::warn!`.
    /// - **wait task** — waits for child exit, emits `ServerExited`.
    pub async fn spawn(
        key: ServerKey,
        cmd: &ServerConfig,
        evt_tx: Sender<LspEvent>,
    ) -> anyhow::Result<Self> {
        let mut child = tokio::process::Command::new(&cmd.command)
            .args(&cmd.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn LSP server {:?}", cmd.command))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // Spawn stdin writer task.
        tokio::spawn(stdin_task(stdin_rx, stdin));

        // Spawn stderr logger task.
        tokio::spawn(stderr_task(stderr, key.language.clone()));

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // We need to perform the initialize handshake before spawning the
        // stdout dispatch loop. We do this by owning the stdout reader here,
        // completing initialize, then handing it off to the dispatch task.
        let capabilities =
            initialize_handshake(&key, &stdin_tx, stdout, evt_tx.clone(), pending.clone()).await?;

        // Spawn wait task so ServerExited is emitted when the child exits.
        spawn_wait_task(child, key.clone(), evt_tx);

        Ok(Self {
            key,
            capabilities,
            stdin_tx,
            next_request_id: 1,
            pending,
        })
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn send_notification(&mut self, method: &str, params: Value) {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.enqueue(msg);
    }

    /// Send a JSON-RPC request, mapping the server's internal id back to
    /// the app-allocated `app_id` when the response arrives.
    ///
    /// The `app_id` is the value that will appear in
    /// `LspEvent::Response { request_id, .. }` so the app can correlate
    /// the response with its `lsp_pending` table without knowing the
    /// server's internal numbering.
    pub fn send_request(&mut self, app_id: i64, method: &str, params: Value) {
        let id = self.next_request_id;
        self.next_request_id += 1;
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, app_id);
        }
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.enqueue(msg);
    }

    /// Gracefully shut down: send `shutdown` request, then `exit` notification,
    /// then drop the stdin sender so the stdin task terminates.
    pub async fn shutdown(mut self) {
        // Use -1 as the app_id for the internal shutdown request — it won't
        // match anything in the app's pending table, which is intentional.
        self.send_request(-1, "shutdown", Value::Null);
        // We don't wait for the response here — just flush and exit.
        tracing::debug!(key = ?self.key, "sent shutdown request");
        self.send_notification("exit", Value::Null);
        // Dropping `stdin_tx` closes the channel; the stdin task will drain
        // remaining messages and exit naturally.
        drop(self.stdin_tx);
    }

    fn enqueue(&self, msg: Value) {
        match serde_json::to_vec(&msg) {
            Ok(bytes) => {
                let _ = self.stdin_tx.send(bytes);
            }
            Err(e) => {
                tracing::warn!("failed to serialize JSON-RPC message: {e}");
            }
        }
    }
}

/// Perform the `initialize` / `initialized` handshake over `stdin_tx` / `stdout`.
///
/// Returns (capabilities, stdout_reader) on success. The caller must hand the
/// stdout reader to the dispatch loop task.
async fn initialize_handshake(
    key: &ServerKey,
    stdin_tx: &mpsc::UnboundedSender<Vec<u8>>,
    stdout: impl AsyncRead + Unpin + Send + 'static,
    evt_tx: Sender<LspEvent>,
    pending: PendingMap,
) -> anyhow::Result<Value> {
    let root_uri = crate::uri::from_path(&key.root).map_err(|_| {
        anyhow::anyhow!(
            "cannot convert workspace root {:?} to file:// URI",
            key.root
        )
    })?;

    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "clientInfo": { "name": "hjkl", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": root_uri.as_str(),
            "capabilities": {
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": false,
                    }
                },
                "workspace": {}
            },
        },
    });
    let bytes = serde_json::to_vec(&init_msg)?;
    stdin_tx.send(bytes).ok();

    // Read the initialize response synchronously.
    let mut reader = BufReader::with_capacity(256 * 1024, stdout);
    let capabilities = loop {
        let raw = codec::read_message(&mut reader)
            .await?
            .ok_or_else(|| anyhow::anyhow!("server closed stdout before initialize response"))?;

        let val: Value = serde_json::from_slice(&raw)?;

        // Skip server-initiated requests/notifications before the response.
        if val.get("id").and_then(Value::as_i64) == Some(0) {
            // This is our initialize response.
            if let Some(err) = val.get("error") {
                bail!("initialize error: {err}");
            }
            let caps = val
                .get("result")
                .and_then(|r| r.get("capabilities"))
                .cloned()
                .unwrap_or(Value::Null);
            break caps;
        }
        // Server-initiated message before our response — log and skip.
        tracing::debug!(
            key = ?key,
            "received server message before initialize response; ignoring"
        );
    };

    // Send `initialized` notification.
    let init_notif = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {},
    });
    let bytes = serde_json::to_vec(&init_notif)?;
    stdin_tx.send(bytes).ok();

    tracing::info!(key = ?key, "LSP server initialized");
    let _ = evt_tx.send(LspEvent::ServerInitialized {
        key: key.clone(),
        capabilities: capabilities.clone(),
    });

    // Spawn the stdout dispatch loop with the remaining reader.
    let key_clone = key.clone();
    tokio::spawn(stdout_task(reader, key_clone, evt_tx, pending));

    Ok(capabilities)
}

/// Spawn a `Server` whose I/O comes from arbitrary `AsyncRead`/`AsyncWrite`
/// streams rather than a real child process. Used in integration tests.
pub async fn spawn_from_io<R, W>(
    key: ServerKey,
    stdin_writer: W,
    stdout_reader: R,
    evt_tx: Sender<LspEvent>,
) -> anyhow::Result<Server>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(stdin_task(stdin_rx, stdin_writer));

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let capabilities =
        initialize_handshake(&key, &stdin_tx, stdout_reader, evt_tx, pending.clone()).await?;

    Ok(Server {
        key,
        capabilities,
        stdin_tx,
        next_request_id: 1,
        pending,
    })
}

/// Task: drain `rx`, write each chunk as a framed message to `stdin`.
async fn stdin_task<W: AsyncWrite + Unpin>(mut rx: mpsc::UnboundedReceiver<Vec<u8>>, mut w: W) {
    while let Some(bytes) = rx.recv().await {
        if let Err(e) = codec::write_message(&mut w, &bytes).await {
            tracing::debug!("LSP stdin write error: {e}");
            break;
        }
    }
}

/// Task: read framed messages from `stdout`, dispatch to `evt_tx`.
async fn stdout_task<R: AsyncRead + Unpin>(
    mut reader: BufReader<R>,
    key: ServerKey,
    evt_tx: Sender<LspEvent>,
    pending: PendingMap,
) {
    loop {
        let raw = match codec::read_message(&mut reader).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                tracing::debug!(key = ?key, "LSP stdout closed (clean EOF)");
                break;
            }
            Err(e) => {
                tracing::warn!(key = ?key, "LSP stdout read error: {e}");
                break;
            }
        };

        let val: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(key = ?key, "LSP: failed to parse JSON frame: {e}");
                continue;
            }
        };

        dispatch_message(&key, val, &evt_tx, &pending);
    }
}

/// Dispatch a decoded JSON-RPC value as either a response or a notification.
fn dispatch_message(key: &ServerKey, val: Value, evt_tx: &Sender<LspEvent>, pending: &PendingMap) {
    let has_id = val.get("id").is_some();
    let has_method = val.get("method").is_some();

    if has_id && !has_method {
        // Response to one of our requests.
        let jsonrpc_id = match val.get("id").and_then(Value::as_i64) {
            Some(i) => i,
            None => {
                tracing::warn!(key = ?key, "LSP response with non-integer id; ignoring");
                return;
            }
        };
        // Map the JSON-RPC id back to the app-allocated request id.
        // If the id is not found (e.g. the internal shutdown request uses -1),
        // drop the response silently.
        let app_id = match pending.lock().ok().and_then(|mut m| m.remove(&jsonrpc_id)) {
            Some(id) => id,
            None => {
                tracing::debug!(key = ?key, jsonrpc_id, "LSP response for unknown id; ignoring");
                return;
            }
        };
        let result = if let Some(err) = val.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            Err(RpcError { code, message })
        } else {
            Ok(val.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = evt_tx.send(LspEvent::Response {
            request_id: app_id,
            result,
        });
    } else if has_method {
        if has_id {
            // Server-initiated request (rare, e.g. workspace/applyEdit).
            // Phase 1: log and ignore.
            let method = val
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            tracing::debug!(key = ?key, method, "LSP server-initiated request; ignoring in Phase 1");
        } else {
            // Push notification (e.g. textDocument/publishDiagnostics).
            let method = val
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>")
                .to_string();
            let params = val.get("params").cloned().unwrap_or(Value::Null);
            tracing::debug!(key = ?key, method, "LSP notification received");
            let _ = evt_tx.send(LspEvent::Notification {
                key: key.clone(),
                method,
                params,
            });
        }
    } else {
        tracing::warn!(key = ?key, "LSP: unrecognized message shape; ignoring");
    }
}

/// Task: read stderr lines and log them as warnings.
async fn stderr_task<R: tokio::io::AsyncRead + Unpin>(stderr: R, lang: String) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    tracing::warn!(lang, "LSP stderr: {trimmed}");
                }
            }
            Err(e) => {
                tracing::debug!(lang, "LSP stderr read error: {e}");
                break;
            }
        }
    }
}

/// Spawn a wait task that emits `ServerExited` when the child exits.
pub fn spawn_wait_task(mut child: Child, key: ServerKey, evt_tx: Sender<LspEvent>) {
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                tracing::info!(key = ?key, ?status, "LSP server exited");
                let _ = evt_tx.send(LspEvent::ServerExited { key, status });
            }
            Err(e) => {
                tracing::warn!(key = ?key, "error waiting for LSP server: {e}");
            }
        }
    });
}
