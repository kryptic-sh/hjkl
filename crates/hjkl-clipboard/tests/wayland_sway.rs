//! Real-compositor integration test against sway in headless wlroots mode.
//!
//! Spawns `sway -c /dev/null` with `WLR_BACKENDS=headless`, scrapes the
//! socket name out of `$XDG_RUNTIME_DIR`, points hjkl-clipboard at it, and
//! exercises the bind handshake + roundtrip set/get.
//!
//! Skips if `sway` is not in `PATH`. Lives in its own test binary so the
//! env mutation doesn't bleed into the in-process unit tests.
//!
//! Why sway and not weston: weston 15 (Arch / Ubuntu 24.10) ships zero
//! data-control protocol support — neither `ext_` nor `zwlr_`. Our backend
//! requires `ext_data_control_manager_v1` to bypass focus-based selection,
//! which sway/wlroots provide alongside the legacy `zwlr_` variant.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hjkl_clipboard::Clipboard;

struct Sway {
    child: Child,
    runtime_dir: PathBuf,
    socket: String,
}

impl Drop for Sway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

fn spawn_sway() -> Option<Sway> {
    if Command::new("sway").arg("--version").output().is_err() {
        eprintln!("SKIP: sway not in PATH");
        return None;
    }

    let runtime_dir = std::env::temp_dir().join(format!("hjkl-sway-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 700");

    let child = Command::new("sway")
        .arg("-c")
        .arg("/dev/null")
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sway");

    // Wrap in Sway immediately so Drop handles cleanup even if the socket
    // never appears (and we panic) — otherwise the spawned process leaks.
    let mut sway = Sway {
        child,
        runtime_dir: runtime_dir.clone(),
        socket: String::new(),
    };

    // wlroots picks the next free wayland-N name. Poll the runtime dir for
    // any non-lock socket file matching wayland-*.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(entries) = std::fs::read_dir(&runtime_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("wayland-") && !name.ends_with(".lock") {
                    sway.socket = name.to_string();
                    // SAFETY: this test binary only mutates env once before
                    // any hjkl-clipboard call. No other thread is reading
                    // env at this point.
                    unsafe {
                        std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
                        std::env::set_var("WAYLAND_DISPLAY", &sway.socket);
                    }
                    // Give sway a beat to finish wiring up globals after
                    // the socket appears.
                    std::thread::sleep(Duration::from_millis(150));
                    return Some(sway);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    panic!("sway socket did not appear in {runtime_dir:?} within 5 s");
}

/// Sway exposes `ext_data_control_manager_v1`; verify our backend selects
/// wayland (not the OSC52 fallback) and the bind handshake completes
/// cleanly. Set/get roundtrip against a self-served selection is tracked
/// separately — it requires the bg thread to drain post-bind events
/// before serving the offer back to ourselves.
#[test]
fn sway_headless_wayland_backend_selected() {
    let Some(sway) = spawn_sway() else {
        return;
    };
    eprintln!("connected to sway socket: {}", sway.socket);

    let cb = Clipboard::new().expect("Clipboard::new against sway headless");
    assert_eq!(
        cb.backend_name(),
        "wayland",
        "expected wayland backend, got {}",
        cb.backend_name()
    );
}
