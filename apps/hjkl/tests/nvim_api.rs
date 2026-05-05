//! Integration tests for `hjkl --nvim-api` msgpack-rpc server mode.
//!
//! Each test spawns the binary, connects nvim-rs as a client, drives it with
//! nvim-compatible method calls, and asserts on the resulting state.

use async_trait::async_trait;
use nvim_rs::{Handler, Neovim, Value, compat::tokio::Compat, create::tokio as create};
use tokio::process::{ChildStdin, Command};

// ── Noop handler (we never receive incoming requests from hjkl) ───────────────

#[derive(Clone)]
struct NoopHandler;

#[async_trait]
impl Handler for NoopHandler {
    type Writer = Compat<ChildStdin>;
}

// ── spawn helper ──────────────────────────────────────────────────────────────

async fn spawn_hjkl_nvim_api() -> anyhow::Result<(
    Neovim<Compat<ChildStdin>>,
    tokio::task::JoinHandle<Result<(), Box<nvim_rs::error::LoopError>>>,
    tokio::process::Child,
)> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hjkl"));
    cmd.arg("--nvim-api");
    let (nvim, io_handle, child) = create::new_child_cmd(&mut cmd, NoopHandler).await?;
    Ok((nvim, io_handle, child))
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Round-trip: set_lines(["hello"]) → get_lines() == ["hello"]
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_set_get_lines_roundtrip() {
    let (nvim, _io, mut child) = spawn_hjkl_nvim_api().await.expect("spawn hjkl --nvim-api");

    let buf = nvim.get_current_buf().await.expect("get_current_buf");
    buf.set_lines(0, -1, false, vec!["hello".to_string()])
        .await
        .expect("set_lines");

    let lines = buf.get_lines(0, -1, false).await.expect("get_lines");
    assert_eq!(lines, vec!["hello"], "round-trip lines mismatch: {lines:?}");

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// Input: nvim_input("iworld<Esc>") → buffer contains "world"
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_input_inserts_text() {
    let (nvim, _io, mut child) = spawn_hjkl_nvim_api().await.expect("spawn hjkl --nvim-api");

    let buf = nvim.get_current_buf().await.expect("get_current_buf");
    nvim.input("iworld<Esc>").await.expect("nvim_input");

    // Sync barrier.
    let _ = nvim.command("echo 1").await;

    let lines = buf.get_lines(0, -1, false).await.expect("get_lines");
    assert_eq!(
        lines,
        vec!["world"],
        "buffer after input mismatch: {lines:?}"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// Ex command: nvim_command(":%s/foo/bar/g") on buffer "foo" → "bar"
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_command_substitute() {
    let (nvim, _io, mut child) = spawn_hjkl_nvim_api().await.expect("spawn hjkl --nvim-api");

    let buf = nvim.get_current_buf().await.expect("get_current_buf");
    buf.set_lines(0, -1, false, vec!["foo".to_string()])
        .await
        .expect("set_lines");

    nvim.command(":%s/foo/bar/g").await.expect("nvim_command");

    let lines = buf.get_lines(0, -1, false).await.expect("get_lines");
    assert_eq!(
        lines,
        vec!["bar"],
        "buffer after substitute mismatch: {lines:?}"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// Cursor: set_cursor((1,2)) → get_cursor() == (1,2) (1-based row)
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_cursor_roundtrip() {
    let (nvim, _io, mut child) = spawn_hjkl_nvim_api().await.expect("spawn hjkl --nvim-api");

    let buf = nvim.get_current_buf().await.expect("get_current_buf");
    buf.set_lines(0, -1, false, vec!["hello world".to_string()])
        .await
        .expect("set_lines");

    let win = nvim.get_current_win().await.expect("get_current_win");
    win.set_cursor((1, 2)).await.expect("set_cursor");

    let (row, col) = win.get_cursor().await.expect("get_cursor");
    assert_eq!(row, 1, "cursor row should be 1, got {row}");
    assert_eq!(col, 2, "cursor col should be 2, got {col}");

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// Mode: after nvim_input("i") → get_mode().mode == "i"
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_mode_transitions() {
    let (nvim, _io, mut child) = spawn_hjkl_nvim_api().await.expect("spawn hjkl --nvim-api");

    // Initial mode should be normal ("n").
    let pairs = nvim.get_mode().await.expect("get_mode initial");
    let mode = pairs
        .into_iter()
        .find_map(|(k, v)| {
            if k == Value::from("mode") {
                v.as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .unwrap_or_default();
    assert_eq!(mode, "n", "initial mode should be 'n', got: {mode:?}");

    // Enter insert mode.
    nvim.input("i").await.expect("nvim_input 'i'");

    let pairs = nvim.get_mode().await.expect("get_mode after i");
    let mode = pairs
        .into_iter()
        .find_map(|(k, v)| {
            if k == Value::from("mode") {
                v.as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .unwrap_or_default();
    assert_eq!(mode, "i", "mode after 'i' should be 'i', got: {mode:?}");

    let _ = child.kill().await;
    let _ = child.wait().await;
}
