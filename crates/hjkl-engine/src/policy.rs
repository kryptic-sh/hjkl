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
