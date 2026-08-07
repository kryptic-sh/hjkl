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

// ── filesystem policy (RPC confinement) ──────────────────────────────────────

/// Spawn `hjkl --nvim-api` with the given directory as its working directory —
/// the FS policy confines `:e`/`:w`/`:r` to that subtree, so the child must
/// start there.
async fn spawn_hjkl_nvim_api_in(
    dir: &std::path::Path,
) -> anyhow::Result<(
    Neovim<Compat<ChildStdin>>,
    tokio::task::JoinHandle<Result<(), Box<nvim_rs::error::LoopError>>>,
    tokio::process::Child,
)> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hjkl"));
    cmd.arg("--nvim-api");
    cmd.current_dir(dir);
    let (nvim, io_handle, child) = create::new_child_cmd(&mut cmd, NoopHandler).await?;
    Ok((nvim, io_handle, child))
}

/// FS policy in `--nvim-api` mode: `:e` of an absolute path outside the
/// working directory must be refused — the file's content must never reach the
/// buffer (the command itself returns Ok; the refusal is observable in the
/// buffer) — while a relative path inside the working directory still opens.
/// Pins the `do_edit` confinement gate.
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_edit_is_confined_to_cwd() {
    let td = tempfile::tempdir().expect("tempdir");
    std::fs::write(td.path().join("ok.txt"), "inside line\n").expect("write ok.txt");
    // A guaranteed-existing file OUTSIDE the working directory, with known
    // content, so the negative assertion does not depend on the host's
    // filesystem layout.
    let outside = tempfile::tempdir().expect("outside tempdir");
    let secret_path = outside.path().join("secret.txt");
    std::fs::write(&secret_path, "TOP SECRET\n").expect("write secret");

    let (nvim, _io, mut child) = spawn_hjkl_nvim_api_in(td.path())
        .await
        .expect("spawn hjkl --nvim-api");

    // Negative: absolute path outside cwd → refused, buffer untouched.
    let buf = nvim.get_current_buf().await.expect("get_current_buf");
    let before = buf.get_lines(0, -1, false).await.expect("get_lines before");
    nvim.command(&format!(":e {}", secret_path.display()))
        .await
        .expect("nvim_command :e <outside path>");
    let buf = nvim
        .get_current_buf()
        .await
        .expect("get_current_buf after refused :e");
    let after = buf.get_lines(0, -1, false).await.expect("get_lines after");
    assert_eq!(
        before, after,
        "refused :e must leave the buffer untouched, got: {after:?}"
    );
    assert!(
        !after.iter().any(|l| l.contains("TOP SECRET")),
        "refused :e leaked outside content into the buffer: {after:?}"
    );

    // Positive: relative path inside cwd still opens — confinement must not
    // become a blanket refusal.
    nvim.command(":e ok.txt")
        .await
        .expect("nvim_command :e ok.txt");
    let buf = nvim
        .get_current_buf()
        .await
        .expect("get_current_buf after :e ok.txt");
    let lines = buf.get_lines(0, -1, false).await.expect("get_lines ok.txt");
    assert_eq!(
        lines,
        vec!["inside line"],
        "confinement became a blanket refusal: {lines:?}"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// `:e` through a symlink pointing outside the working directory must be
/// refused — pins the `resolve_under` half of the confinement, which
/// `check_fs_path` alone cannot provide (`escape/secret.txt` is all `Normal`
/// components and passes the lexical check).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_edit_through_symlink_escape_is_refused() {
    let td = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    std::fs::write(outside.path().join("secret.txt"), "TOP SECRET\n").expect("write secret");
    std::os::unix::fs::symlink(outside.path(), td.path().join("escape")).expect("symlink");

    let (nvim, _io, mut child) = spawn_hjkl_nvim_api_in(td.path())
        .await
        .expect("spawn hjkl --nvim-api");

    let _buf = nvim.get_current_buf().await.expect("get_current_buf");
    nvim.command(":e escape/secret.txt")
        .await
        .expect("nvim_command :e escape/secret.txt");
    let buf = nvim
        .get_current_buf()
        .await
        .expect("get_current_buf after refused :e");
    let lines = buf.get_lines(0, -1, false).await.expect("get_lines");
    assert!(
        !lines.iter().any(|l| l.contains("TOP SECRET")),
        ":e through a symlink escape leaked content into the buffer: {lines:?}"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}
