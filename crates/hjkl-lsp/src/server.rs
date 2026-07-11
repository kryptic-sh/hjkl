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

/// How long to wait for the `initialize` response before giving up. A silent
/// server would otherwise block the single-threaded dispatch loop forever.
const INITIALIZE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long to wait after `shutdown`/`exit` for the child to exit on its own
/// before force-killing it.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Signals the wait task to force-kill the child (graceful shutdown grace
    /// period expired). `None` for test servers with no real child.
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Handle to the wait task; awaited during shutdown so the child is
    /// reaped before we return. `None` for test servers with no real child.
    wait_handle: Option<tokio::task::JoinHandle<()>>,
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
            .kill_on_drop(true)
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
        // Effective initializationOptions: explicit config wins, otherwise a
        // per-server default (rust-analyzer → run clippy on save).
        let init_options = cmd
            .initialization_options
            .clone()
            .or_else(|| default_init_options(&cmd.command));
        let capabilities = match initialize_handshake(
            &key,
            &stdin_tx,
            stdout,
            evt_tx.clone(),
            pending.clone(),
            init_options.as_ref(),
        )
        .await
        {
            Ok(caps) => caps,
            Err(e) => {
                // Handshake failed (garbage frames, closed stdout, …) — kill
                // and reap the child so it doesn't linger as an orphan.
                let _ = child.start_kill();
                let _ = child.wait().await;
                return Err(e);
            }
        };

        // Spawn wait task so ServerExited is emitted when the child exits.
        let (kill_tx, wait_handle) = spawn_wait_task(child, key.clone(), evt_tx);

        Ok(Self {
            key,
            capabilities,
            stdin_tx,
            next_request_id: 1,
            pending,
            kill_tx: Some(kill_tx),
            wait_handle: Some(wait_handle),
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
    /// then drop the stdin sender so the stdin task terminates. If the child
    /// doesn't exit within [`SHUTDOWN_GRACE`], force-kill and reap it so a
    /// server that ignores `exit` cannot linger as an orphan.
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

        // Wait for the child to exit gracefully; force-kill after the grace
        // period. Both fields are `None` on the `spawn_from_io` (test) path.
        if let (Some(mut handle), Some(kill_tx)) = (self.wait_handle.take(), self.kill_tx.take()) {
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut handle).await {
                Ok(_) => {} // wait task finished => child exited gracefully
                Err(_) => {
                    tracing::warn!(
                        key = ?self.key,
                        "LSP server did not exit within {SHUTDOWN_GRACE:?}; force-killing"
                    );
                    let _ = kill_tx.send(()); // signal force-kill
                    let _ = handle.await; // wait for the kill+reap to finish
                }
            }
        }
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

/// Per-server default `initializationOptions` when the user hasn't set any.
///
/// For rust-analyzer this turns the on-save flycheck into `cargo clippy` (both
/// the modern `check.command` and the legacy `checkOnSave.command` keys, for
/// version compatibility) so clippy lints surface as diagnostics while editing.
fn default_init_options(command: &str) -> Option<Value> {
    // Match on the executable's file stem so `/path/to/rust-analyzer` works too.
    let stem = std::path::Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(command);
    if stem == "rust-analyzer" {
        Some(json!({
            "check": { "command": "clippy" },
            "checkOnSave": { "command": "clippy" },
        }))
    } else {
        None
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
    init_options: Option<&Value>,
) -> anyhow::Result<Value> {
    let root_uri = crate::uri::from_path(&key.root).map_err(|_| {
        anyhow::anyhow!(
            "cannot convert workspace root {:?} to file:// URI",
            key.root
        )
    })?;

    let mut params = json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "hjkl", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": root_uri.as_str(),
        "capabilities": {
            // Request UTF-8 byte positions; fall back to UTF-16 if the
            // server doesn't support it. Asking lets servers (like
            // rust-analyzer) skip per-line UTF-16 column conversion.
            "general": {
                "positionEncodings": ["utf-8", "utf-16"],
            },
            "textDocument": {
                "synchronization": {
                    "dynamicRegistration": false,
                    "willSave": false,
                    "willSaveWaitUntil": false,
                    // Announce didSave so the server runs its on-save flycheck
                    // (e.g. rust-analyzer's `cargo clippy`) — without this the
                    // client never sends didSave and clippy never re-runs.
                    "didSave": true,
                }
            },
            "workspace": {}
        },
    });
    // Server-specific initializationOptions (e.g. rust-analyzer clippy config).
    if let (Some(opts), Some(obj)) = (init_options, params.as_object_mut()) {
        obj.insert("initializationOptions".to_string(), opts.clone());
    }
    let init_msg = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": params,
    });
    let bytes = serde_json::to_vec(&init_msg)?;
    stdin_tx.send(bytes).ok();

    // Read the initialize response synchronously, bounded by a timeout so a
    // silent server cannot block the dispatch loop forever.
    let mut reader = BufReader::with_capacity(256 * 1024, stdout);
    let capabilities = tokio::time::timeout(INITIALIZE_TIMEOUT, async {
        loop {
            let raw = codec::read_message(&mut reader).await?.ok_or_else(|| {
                anyhow::anyhow!("server closed stdout before initialize response")
            })?;

            let val: Value = serde_json::from_slice(&raw)?;

            // Skip server-initiated requests/notifications before the response.
            // A response has an `id` and no `method`; the `method` check matters
            // because a server-initiated *request* may legally also use id 0.
            if val.get("id").and_then(Value::as_i64) == Some(0) && val.get("method").is_none() {
                // This is our initialize response.
                if let Some(err) = val.get("error") {
                    bail!("initialize error: {err}");
                }
                let caps = val
                    .get("result")
                    .and_then(|r| r.get("capabilities"))
                    .cloned()
                    .unwrap_or(Value::Null);
                break Ok::<Value, anyhow::Error>(caps);
            }
            // Server-initiated message before our response — log and skip.
            tracing::debug!(
                key = ?key,
                "received server message before initialize response; ignoring"
            );
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "initialize handshake timed out after {}s",
            INITIALIZE_TIMEOUT.as_secs()
        )
    })??;

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

    // Spawn the stdout dispatch loop with the remaining reader. It gets a
    // clone of the stdin sender so server-initiated requests can be answered.
    let key_clone = key.clone();
    tokio::spawn(stdout_task(
        reader,
        key_clone,
        evt_tx,
        pending,
        stdin_tx.clone(),
    ));

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
    let capabilities = initialize_handshake(
        &key,
        &stdin_tx,
        stdout_reader,
        evt_tx,
        pending.clone(),
        None,
    )
    .await?;

    Ok(Server {
        key,
        capabilities,
        stdin_tx,
        next_request_id: 1,
        pending,
        kill_tx: None,
        wait_handle: None,
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
///
/// `stdin_tx` is used to answer server-initiated requests so servers that
/// block on them (e.g. rust-analyzer) don't deadlock.
async fn stdout_task<R: AsyncRead + Unpin>(
    mut reader: BufReader<R>,
    key: ServerKey,
    evt_tx: Sender<LspEvent>,
    pending: PendingMap,
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
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

        dispatch_message(&key, val, &evt_tx, &pending, &stdin_tx);
    }
}

/// Dispatch a decoded JSON-RPC value as a response, a server-initiated
/// request (auto-answered via `stdin_tx`), or a notification.
fn dispatch_message(
    key: &ServerKey,
    val: Value,
    evt_tx: &Sender<LspEvent>,
    pending: &PendingMap,
    stdin_tx: &mpsc::UnboundedSender<Vec<u8>>,
) {
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
            // Server-initiated request (e.g. workspace/configuration). Every
            // request MUST get a response echoing the same id — servers like
            // rust-analyzer block waiting for one.
            let id = val.get("id").cloned().unwrap_or(Value::Null);
            let method = val
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            match method {
                "workspace/configuration" => {
                    // One `null` per requested item — "no configuration".
                    let count = val
                        .get("params")
                        .and_then(|p| p.get("items"))
                        .and_then(Value::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0);
                    send_response(stdin_tx, id, Value::Array(vec![Value::Null; count]));
                }
                "client/registerCapability"
                | "client/unregisterCapability"
                | "window/workDoneProgress/create" => {
                    send_response(stdin_tx, id, Value::Null);
                }
                "workspace/applyEdit" => {
                    // We deliberately do NOT apply edits from the I/O task.
                    send_response(stdin_tx, id, json!({ "applied": false }));
                }
                _ => {
                    send_error_response(stdin_tx, id, -32601, "method not supported");
                }
            }
            tracing::debug!(key = ?key, method, "LSP server-initiated request auto-answered");
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

/// Enqueue a JSON-RPC success response (used to answer server-initiated
/// requests from the stdout dispatch task).
fn send_response(stdin_tx: &mpsc::UnboundedSender<Vec<u8>>, id: Value, result: Value) {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    if let Ok(bytes) = serde_json::to_vec(&msg) {
        let _ = stdin_tx.send(bytes);
    }
}

/// Enqueue a JSON-RPC error response (used to answer server-initiated
/// requests for methods we don't support).
fn send_error_response(
    stdin_tx: &mpsc::UnboundedSender<Vec<u8>>,
    id: Value,
    code: i64,
    message: &str,
) {
    let msg = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    if let Ok(bytes) = serde_json::to_vec(&msg) {
        let _ = stdin_tx.send(bytes);
    }
}

/// Task: read stderr lines and log them as warnings.
///
/// Each read is capped so a server spewing an endless line with no newline
/// cannot grow the buffer without bound (overlong lines are logged in
/// chunks), and non-UTF-8 output is logged lossily instead of killing the
/// logger task.
async fn stderr_task<R: tokio::io::AsyncRead + Unpin>(stderr: R, lang: String) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    const MAX_LINE_BYTES: u64 = 8 * 1024;
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = {
            let mut limited = (&mut reader).take(MAX_LINE_BYTES);
            match limited.read_until(b'\n', &mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::debug!(lang, "LSP stderr read error: {e}");
                    break;
                }
            }
        };
        if n == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&buf);
        let trimmed = text.trim_end();
        if !trimmed.is_empty() {
            tracing::warn!(lang, "LSP stderr: {trimmed}");
        }
    }
}

/// Spawn a wait task that emits `ServerExited` when the child exits.
///
/// Returns a oneshot sender that force-kills the child when fired (used by
/// `Server::shutdown` after the grace period) and the task's join handle so
/// shutdown can await the kill+reap.
fn spawn_wait_task(
    mut child: Child,
    key: ServerKey,
    evt_tx: Sender<LspEvent>,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let (kill_tx, kill_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        tokio::select! {
            res = child.wait() => {
                match res {
                    Ok(status) => {
                        tracing::info!(key = ?key, ?status, "LSP server exited");
                        let _ = evt_tx.send(LspEvent::ServerExited { key, status });
                    }
                    Err(e) => {
                        tracing::warn!(key = ?key, "error waiting for LSP server: {e}");
                    }
                }
            }
            _ = kill_rx => {
                let _ = child.start_kill();
                match child.wait().await {
                    Ok(status) => {
                        tracing::info!(key = ?key, ?status, "LSP server force-killed on shutdown");
                        let _ = evt_tx.send(LspEvent::ServerExited { key, status });
                    }
                    Err(e) => {
                        tracing::warn!(key = ?key, "error waiting for force-killed LSP server: {e}");
                    }
                }
            }
        }
    });
    (kill_tx, handle)
}
