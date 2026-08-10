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

/// Spawn `hjkl --nvim-api` with `XDG_DATA_HOME` / `XDG_CACHE_HOME` pointed at
/// the given dirs, so the anvil store resolves inside the test's tempdir
/// instead of the developer's real home.
async fn spawn_hjkl_nvim_api_with_xdg(
    data_home: &std::path::Path,
    cache_home: &std::path::Path,
) -> anyhow::Result<(
    Neovim<Compat<ChildStdin>>,
    tokio::task::JoinHandle<Result<(), Box<nvim_rs::error::LoopError>>>,
    tokio::process::Child,
)> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hjkl"));
    cmd.arg("--nvim-api");
    cmd.env("XDG_DATA_HOME", data_home);
    cmd.env("XDG_CACHE_HOME", cache_home);
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

/// Read-side confinement: a buffer RENAMED via `nvim_buf_set_name` to an
/// absolute path outside the working directory must not be readable back
/// through a bare `:e` reload. The renamed filename is stored verbatim and
/// `do_edit`'s no-arg path (`reload_current`) skips the `:e <path>` gate, so
/// the read itself must be policy-checked. The refusal is observable in the
/// buffer (the command returns Ok; the policy error goes to the app's message
/// bus, which the RPC protocol does not expose). Also pins the no-regression
/// side: reloading a file opened inside the cwd — whose stored filename is the
/// RESOLVED absolute path, which the lexical `check_fs_path` alone would
/// refuse — must still work.
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_bare_e_reload_of_renamed_outside_path_is_refused() {
    let td = tempfile::tempdir().expect("tempdir");
    std::fs::write(td.path().join("ok.txt"), "inside line\n").expect("write ok.txt");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let secret_path = outside.path().join("secret.txt");
    std::fs::write(&secret_path, "TOP SECRET\n").expect("write secret");

    let (nvim, _io, mut child) = spawn_hjkl_nvim_api_in(td.path())
        .await
        .expect("spawn hjkl --nvim-api");

    // Positive control: open a file inside the cwd, then bare-`:e` reload it.
    // `:e ok.txt` stores the resolved absolute path as the slot filename, so a
    // working reload here proves the gate isn't a blanket absolute-path refusal.
    nvim.command(":e ok.txt")
        .await
        .expect("nvim_command :e ok.txt");
    let buf = nvim
        .get_current_buf()
        .await
        .expect("get_current_buf after :e ok.txt");
    nvim.command("e")
        .await
        .expect("nvim_command e (legit reload)");
    let lines = buf
        .get_lines(0, -1, false)
        .await
        .expect("get_lines after legit reload");
    assert_eq!(
        lines,
        vec!["inside line"],
        "legit bare :e reload of an inside file must still work: {lines:?}"
    );

    // Attack: rename the buffer to an absolute path outside the cwd.
    buf.set_name(secret_path.to_str().unwrap())
        .await
        .expect("nvim_buf_set_name outside cwd");
    let before = buf
        .get_lines(0, -1, false)
        .await
        .expect("get_lines before attack :e");

    nvim.command("e").await.expect("nvim_command e");
    let after = buf
        .get_lines(0, -1, false)
        .await
        .expect("get_lines after attack :e");
    assert_eq!(
        before, after,
        "refused :e reload must leave the buffer untouched, got: {after:?}"
    );
    assert!(
        !after.iter().any(|l| l.contains("TOP SECRET")),
        "bare :e reload leaked the renamed outside path into the buffer: {after:?}"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}

/// Same read-side gap via `:checktime`: `checktime_slot`'s autoreload reads the
/// slot filename with no policy check, so a buffer renamed outside the cwd
/// would be auto-reloaded from the outside file. Pins the checktime half.
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_checktime_autoreload_of_renamed_outside_path_is_refused() {
    let td = tempfile::tempdir().expect("tempdir");
    std::fs::write(td.path().join("ok.txt"), "inside line\n").expect("write ok.txt");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let secret_path = outside.path().join("secret.txt");
    std::fs::write(&secret_path, "TOP SECRET\n").expect("write secret");

    let (nvim, _io, mut child) = spawn_hjkl_nvim_api_in(td.path())
        .await
        .expect("spawn hjkl --nvim-api");

    nvim.command(":e ok.txt")
        .await
        .expect("nvim_command :e ok.txt");
    let buf = nvim
        .get_current_buf()
        .await
        .expect("get_current_buf after :e ok.txt");
    let before = buf
        .get_lines(0, -1, false)
        .await
        .expect("get_lines before rename");
    assert_eq!(before, vec!["inside line"], "precondition: {before:?}");

    // Rename to an absolute path outside the cwd, then let `:checktime` try to
    // autoreload it.
    buf.set_name(secret_path.to_str().unwrap())
        .await
        .expect("nvim_buf_set_name outside cwd");
    nvim.command("checktime")
        .await
        .expect("nvim_command checktime");
    let after = buf
        .get_lines(0, -1, false)
        .await
        .expect("get_lines after checktime");
    assert_eq!(
        before, after,
        "refused :checktime must leave the buffer untouched, got: {after:?}"
    );
    assert!(
        !after.iter().any(|l| l.contains("TOP SECRET")),
        ":checktime autoreload leaked the renamed outside path into the buffer: {after:?}"
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

/// `:Anvil install` is gated on the shell policy in `--nvim-api` mode: the
/// install pipeline downloads archives and runs package-manager build scripts
/// (`cargo`/`npm`/`pip`/`go`), so an untrusted RPC client must not be able to
/// trigger it without `--allow-shell`. The command returns Ok either way (the
/// `ExEffect::Error` goes to the app's message bus, which the RPC protocol
/// does not expose), so the observable is on disk: `install_blocking`'s first
/// I/O is `create_dir_all` on `<data>/anvil/packages` and
/// `<data>/anvil/checksums`, then the staging dir under `<cache>/anvil` — all
/// before any network fetch. None of those may appear. (Startup does create
/// `<data>/anvil/bin` via the PATH prepend, so the assertion is on the
/// install-specific dirs, not the whole store; the `bin` dir's presence is
/// what proves the child honored the injected XDG roots.) Picks
/// `rust-analyzer` — a real Github-method tool from the embedded registry — so
/// the install WOULD start if the gate were missing; the poll only waits for
/// the pre-fix directory creation, never for the download to finish.
#[tokio::test(flavor = "multi_thread")]
async fn nvim_api_anvil_install_is_refused_without_allow_shell() {
    let td = tempfile::tempdir().expect("tempdir");
    let data_home = td.path().join("data");
    let cache_home = td.path().join("cache");

    let (nvim, _io, mut child) = spawn_hjkl_nvim_api_with_xdg(&data_home, &cache_home)
        .await
        .expect("spawn hjkl --nvim-api");

    // The child's startup PATH-prepend creates `<data>/anvil/bin`; its
    // presence confirms the child resolved XDG to our tempdir, so an install
    // that DID happen would land here (and be caught), not in the real store.
    let bin_dir = data_home.join("anvil").join("bin");
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while !bin_dir.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    assert!(
        bin_dir.exists(),
        "child never resolved XDG_DATA_HOME={} — test setup broken",
        data_home.display()
    );

    nvim.command("Anvil install rust-analyzer")
        .await
        .expect("nvim_command Anvil install");

    // Poll for the pre-fix markers: an install that started creates these
    // dirs within milliseconds of the command returning (they are created
    // before the first network byte). Post-fix they must never appear.
    let packages_dir = data_home.join("anvil").join("packages");
    let checksums_dir = data_home.join("anvil").join("checksums");
    let cache_dir = cache_home.join("anvil");
    let mut saw_install = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if packages_dir.exists() || checksums_dir.exists() || cache_dir.exists() {
            saw_install = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    assert!(
        !saw_install,
        "Anvil install started despite the shell policy: packages={} checksums={} cache={}",
        packages_dir.exists(),
        checksums_dir.exists(),
        cache_dir.exists()
    );
    assert!(
        !packages_dir.exists(),
        "anvil packages dir must not be created after a refused install"
    );
    assert!(
        !checksums_dir.exists(),
        "anvil checksums dir must not be created after a refused install"
    );
    assert!(
        !cache_dir.exists(),
        "anvil cache dir must not be created after a refused install"
    );

    let _ = nvim.command("qa!").await;
    let _ = child.wait().await;
}
