//! Process-global execution policy for non-TUI / RPC modes.
//!
//! Interactive TUI keeps full vim parity (shell-out, unrestricted paths). The
//! non-TUI entry points (`--embed`, `--nvim-api`, `--headless`) may take
//! commands from a remote or automated caller that is not the local user, so
//! they can tighten this policy at startup. Mirrors the one-shot global pattern
//! used by the clipboard-disable path (`host::disable_clipboard_for_rpc`).
//!
//! Flags are set once, before any editor is built, and only ever flip from the
//! permissive default to the restrictive state — never back — so a plain
//! `Relaxed` atomic is sufficient.

use std::sync::atomic::{AtomicBool, Ordering};

/// When `true`, shell-out commands (`:!cmd`, `:[range]!cmd`, `:r !cmd`, and the
/// engine range filter) are refused. Default `false` (allowed, as in vim).
static SHELL_DISABLED: AtomicBool = AtomicBool::new(false);

/// Refuse shell-out for the rest of the process. Call once at RPC/headless
/// startup, before building any editor.
pub fn disable_shell() {
    SHELL_DISABLED.store(true, Ordering::Relaxed);
}

/// True if shell-out has been disabled for this process.
pub fn shell_disabled() -> bool {
    SHELL_DISABLED.load(Ordering::Relaxed)
}

/// When `true`, file I/O paths are confined to the current working directory
/// subtree: absolute paths and paths containing a `..` component are refused.
/// Default `false` (unrestricted, as in vim). The RPC entry points enable this
/// so a remote/automated caller cannot read or write arbitrary filesystem
/// locations via `:w`/`:e`/`:r`.
static FS_RESTRICTED: AtomicBool = AtomicBool::new(false);

/// Confine file I/O to the working-directory subtree for the rest of the
/// process. Call once at RPC startup, before building any editor.
pub fn restrict_fs() {
    FS_RESTRICTED.store(true, Ordering::Relaxed);
}

/// True if filesystem access has been confined for this process.
pub fn fs_restricted() -> bool {
    FS_RESTRICTED.load(Ordering::Relaxed)
}

/// True if `path` would escape a confined working directory: it is absolute, or
/// contains a parent-dir (`..`), root, or prefix component.
pub fn path_escapes(path: &std::path::Path) -> bool {
    use std::path::Component;
    path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// `Err` with a uniform message when `path` is refused under a confined
/// filesystem policy; `Ok(())` when access is allowed (policy off, or the path
/// stays within the working directory).
pub fn check_fs_path(path: &std::path::Path) -> Result<(), String> {
    if fs_restricted() && path_escapes(path) {
        return Err(format!(
            "path {} is outside the working directory (blocked in RPC mode)",
            path.display()
        ));
    }
    Ok(())
}
