//! Integration test: mock LSP server via `tokio::io::duplex`.
//!
//! Verifies the full initialize → didOpen → shutdown → exit sequence
//! without spawning a real child process.

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::Receiver;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, BufReader, duplex};

use hjkl_lsp::codec;
use hjkl_lsp::{LspEvent, ServerKey};

/// Helper: read one framed JSON message from a `duplex` end.
async fn read_json<R: tokio::io::AsyncRead + Unpin>(r: &mut BufReader<R>) -> Value {
    let bytes = codec::read_message(r)
        .await
        .expect("read_json: io error")
        .expect("read_json: clean EOF before message");
    serde_json::from_slice(&bytes).expect("read_json: invalid JSON")
}

/// Helper: write one framed JSON message to a `duplex` end.
async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, val: &Value) {
    let bytes = serde_json::to_vec(val).unwrap();
    codec::write_message(w, &bytes).await.unwrap();
}

/// Poll `Receiver<LspEvent>` until we find an event matching `pred` or timeout.
/// Uses `tokio::time::sleep` to yield between polls so tasks can run.
async fn wait_for_event_async(
    rx: &Receiver<LspEvent>,
    timeout: Duration,
    pred: impl Fn(&LspEvent) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        while let Ok(evt) = rx.try_recv() {
            if pred(&evt) {
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Full sequence: initialize → didOpen (client-side) → shutdown → exit.
///
/// The "driver" side acts as the mock LSP server: it responds to protocol
/// messages that our `Server` actor sends.
#[tokio::test(flavor = "current_thread")]
async fn mock_server_full_sequence() {
    tokio::time::timeout(Duration::from_millis(200), async {
        // Duplex pair — `client_io` is what our Server reads/writes.
        // `driver_io` is what the mock server logic drives.
        let (client_io, driver_io) = duplex(64 * 1024);

        let (evt_tx, _evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let key = ServerKey {
            language: "rust".to_string(),
            root: PathBuf::from("/tmp/mock-workspace"),
        };

        // Split both ends.
        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        // Spawn the actor in the background — it will block on initialize.
        let server_fut = tokio::spawn({
            let key = key.clone();
            async move {
                hjkl_lsp::testing::spawn_server_from_io(key, client_write, client_read, evt_tx)
                    .await
            }
        });

        // ── Step 1: driver reads initialize request ──────────────────────────
        let init_req = read_json(&mut driver_reader).await;
        assert_eq!(init_req["method"], "initialize");
        assert_eq!(init_req["jsonrpc"], "2.0");
        let req_id = init_req["id"].as_i64().unwrap();

        // ── Step 2: driver sends initialize response ─────────────────────────
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "capabilities": {
                        "textDocumentSync": 1,
                        "completionProvider": null,
                    }
                }
            }),
        )
        .await;

        // ── Step 3: driver reads initialized notification ────────────────────
        let init_notif = read_json(&mut driver_reader).await;
        assert_eq!(init_notif["method"], "initialized");
        assert!(
            init_notif.get("id").is_none(),
            "initialized must be a notification (no id)"
        );

        // The Server actor should now be ready — collect it.
        let mut server = server_fut
            .await
            .expect("task panicked")
            .expect("spawn_from_io failed");

        // Verify capabilities.
        assert_eq!(server.capabilities["textDocumentSync"], 1);

        // ── Step 4: client sends textDocument/didOpen ────────────────────────
        let path = PathBuf::from("/tmp/mock-workspace/src/main.rs");
        let uri = hjkl_lsp::uri::from_path(&path).unwrap();
        server.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri.as_str(),
                    "languageId": "rust",
                    "version": 1,
                    "text": "fn main() {}",
                }
            }),
        );
        let did_open = read_json(&mut driver_reader).await;
        assert_eq!(did_open["method"], "textDocument/didOpen");
        assert_eq!(did_open["params"]["textDocument"]["languageId"], "rust");
        assert_eq!(did_open["params"]["textDocument"]["text"], "fn main() {}");
        assert_eq!(did_open["params"]["textDocument"]["version"], 1);

        // ── Step 5: client shuts down ────────────────────────────────────────
        server.shutdown().await;

        // ── Step 5a: driver reads shutdown request ───────────────────────────
        let shutdown_req = read_json(&mut driver_reader).await;
        assert_eq!(shutdown_req["method"], "shutdown");
        let shutdown_id = shutdown_req["id"].as_i64().unwrap();

        // ── Step 6: driver sends shutdown response ───────────────────────────
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": shutdown_id,
                "result": null,
            }),
        )
        .await;

        // ── Step 7: driver reads exit notification ───────────────────────────
        let exit_notif = read_json(&mut driver_reader).await;
        assert_eq!(exit_notif["method"], "exit");

        // ── Step 8: close driver side ────────────────────────────────────────
        drop(driver_write);
        drop(driver_reader);
    })
    .await
    .expect("mock_server_full_sequence timed out");
}

/// `ServerInitialized` event is emitted after the handshake completes.
#[tokio::test(flavor = "current_thread")]
async fn server_initialized_event_emitted() {
    tokio::time::timeout(Duration::from_millis(200), async {
        let (client_io, driver_io) = duplex(64 * 1024);
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let key = ServerKey {
            language: "typescript".to_string(),
            root: PathBuf::from("/tmp/ts-project"),
        };

        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        let server_task = tokio::spawn({
            let key = key.clone();
            async move {
                hjkl_lsp::testing::spawn_server_from_io(key, client_write, client_read, evt_tx)
                    .await
            }
        });

        // Respond to initialize.
        let req = read_json(&mut driver_reader).await;
        let req_id = req["id"].as_i64().unwrap();
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": { "capabilities": { "hoverProvider": true } }
            }),
        )
        .await;

        // Read initialized notification.
        let notif = read_json(&mut driver_reader).await;
        assert_eq!(notif["method"], "initialized");

        let _server = server_task.await.unwrap().unwrap();

        // ServerInitialized event must have been emitted.
        assert!(
            wait_for_event_async(&evt_rx, Duration::from_millis(100), |e| matches!(
                e,
                LspEvent::ServerInitialized { .. }
            ))
            .await,
            "ServerInitialized event not received"
        );
    })
    .await
    .expect("server_initialized_event_emitted timed out");
}

/// A notification pushed by the server (e.g. publishDiagnostics) is forwarded
/// as an `LspEvent::Notification`.
#[tokio::test(flavor = "current_thread")]
async fn server_push_notification_forwarded() {
    tokio::time::timeout(Duration::from_millis(200), async {
        let (client_io, driver_io) = duplex(64 * 1024);
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let key = ServerKey {
            language: "go".to_string(),
            root: PathBuf::from("/tmp/go-project"),
        };

        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        let server_task = tokio::spawn({
            let key = key.clone();
            async move {
                hjkl_lsp::testing::spawn_server_from_io(key, client_write, client_read, evt_tx)
                    .await
            }
        });

        // Initialize handshake.
        let req = read_json(&mut driver_reader).await;
        let req_id = req["id"].as_i64().unwrap();
        write_json(
            &mut driver_write,
            &json!({ "jsonrpc": "2.0", "id": req_id, "result": { "capabilities": {} } }),
        )
        .await;
        let _ = read_json(&mut driver_reader).await; // initialized

        let _server = server_task.await.unwrap().unwrap();

        // Driver pushes a notification.
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": { "uri": "file:///tmp/go-project/main.go", "diagnostics": [] }
            }),
        )
        .await;

        // Should arrive as LspEvent::Notification.
        assert!(
            wait_for_event_async(&evt_rx, Duration::from_millis(100), |e| matches!(
                e,
                LspEvent::Notification { method, .. } if method == "textDocument/publishDiagnostics"
            ))
            .await,
            "Notification event not received"
        );
    })
    .await
    .expect("server_push_notification_forwarded timed out");
}

/// A request sent via `Server::send_request` is forwarded to the mock server,
/// and the response arrives as `LspEvent::Response` with the app-allocated id.
#[tokio::test(flavor = "current_thread")]
async fn request_response_roundtrip() {
    tokio::time::timeout(Duration::from_millis(500), async {
        let (client_io, driver_io) = duplex(64 * 1024);
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let key = ServerKey {
            language: "rust".to_string(),
            root: std::path::PathBuf::from("/tmp/rr-workspace"),
        };

        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        let server_task = tokio::spawn({
            let key = key.clone();
            async move {
                hjkl_lsp::testing::spawn_server_from_io(key, client_write, client_read, evt_tx)
                    .await
            }
        });

        // Complete the initialize handshake.
        let req = read_json(&mut driver_reader).await;
        let req_id = req["id"].as_i64().unwrap();
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": { "capabilities": { "definitionProvider": true } }
            }),
        )
        .await;
        let _notif = read_json(&mut driver_reader).await; // initialized

        let mut server = server_task.await.unwrap().unwrap();

        // App sends a textDocument/definition request with app_id = 42.
        let app_id: i64 = 42;
        server.send_request(
            app_id,
            "textDocument/definition",
            json!({
                "textDocument": { "uri": "file:///tmp/rr-workspace/src/main.rs" },
                "position": { "line": 5, "character": 10 },
            }),
        );

        // Driver reads the request — the jsonrpc id is server-internal.
        let def_req = read_json(&mut driver_reader).await;
        assert_eq!(def_req["method"], "textDocument/definition");
        let jsonrpc_id = def_req["id"].as_i64().expect("request must have an id");

        // Driver replies with a location.
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": jsonrpc_id,
                "result": [{
                    "uri": "file:///tmp/rr-workspace/src/lib.rs",
                    "range": {
                        "start": { "line": 10, "character": 4 },
                        "end":   { "line": 10, "character": 12 },
                    }
                }]
            }),
        )
        .await;

        // The response must arrive with the APP-allocated id (42), not the
        // server's internal jsonrpc id.
        assert!(
            wait_for_event_async(&evt_rx, Duration::from_millis(200), |e| matches!(
                e,
                LspEvent::Response { request_id, result: Ok(_) }
                    if *request_id == app_id
            ))
            .await,
            "LspEvent::Response with app_id={app_id} not received"
        );

        drop(driver_write);
        drop(driver_reader);
    })
    .await
    .expect("request_response_roundtrip timed out");
}

/// When the mock server replies with a JSON-RPC error, the response arrives
/// as `LspEvent::Response { result: Err(RpcError { .. }) }`.
#[tokio::test(flavor = "current_thread")]
async fn request_with_error_returns_rpc_error() {
    tokio::time::timeout(Duration::from_millis(500), async {
        let (client_io, driver_io) = duplex(64 * 1024);
        let (evt_tx, evt_rx) = crossbeam_channel::unbounded::<LspEvent>();

        let key = ServerKey {
            language: "python".to_string(),
            root: std::path::PathBuf::from("/tmp/err-workspace"),
        };

        let (driver_read, mut driver_write) = tokio::io::split(driver_io);
        let mut driver_reader = BufReader::with_capacity(256 * 1024, driver_read);
        let (client_read, client_write) = tokio::io::split(client_io);

        let server_task = tokio::spawn({
            let key = key.clone();
            async move {
                hjkl_lsp::testing::spawn_server_from_io(key, client_write, client_read, evt_tx)
                    .await
            }
        });

        let req = read_json(&mut driver_reader).await;
        let req_id = req["id"].as_i64().unwrap();
        write_json(
            &mut driver_write,
            &json!({ "jsonrpc": "2.0", "id": req_id, "result": { "capabilities": {} } }),
        )
        .await;
        let _notif = read_json(&mut driver_reader).await; // initialized

        let mut server = server_task.await.unwrap().unwrap();

        let app_id: i64 = 7;
        server.send_request(
            app_id,
            "textDocument/hover",
            json!({ "textDocument": { "uri": "file:///tmp/err-workspace/main.py" }, "position": { "line": 0, "character": 0 } }),
        );

        // Read the hover request.
        let hover_req = read_json(&mut driver_reader).await;
        let jid = hover_req["id"].as_i64().unwrap();

        // Reply with an error.
        write_json(
            &mut driver_write,
            &json!({
                "jsonrpc": "2.0",
                "id": jid,
                "error": { "code": -32601, "message": "method not supported" }
            }),
        )
        .await;

        assert!(
            wait_for_event_async(&evt_rx, Duration::from_millis(200), |e| matches!(
                e,
                LspEvent::Response { request_id, result: Err(_) }
                    if *request_id == app_id
            ))
            .await,
            "LspEvent::Response Err with app_id={app_id} not received"
        );

        drop(driver_write);
        drop(driver_reader);
    })
    .await
    .expect("request_with_error_returns_rpc_error timed out");
}
