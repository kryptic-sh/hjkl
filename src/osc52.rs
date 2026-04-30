//! OSC 52 terminal clipboard escape sequence helpers.
//!
//! Detects SSH sessions and tmux, wraps payloads in the appropriate escape
//! sequences, and writes them to stdout.

use std::io::{self, Write};

use crate::base64::base64_encode;

/// Spec minimum is 8 KiB; xterm default is 1 MiB. 74 000 bytes is a widely
/// accepted safe cap and matches most terminal implementations.
pub(crate) const OSC52_MAX: usize = 74_000;

/// Returns `true` when the process is running inside an SSH session.
pub(crate) fn is_over_ssh() -> bool {
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
}

/// Returns `true` when the process is running inside tmux.
pub(crate) fn is_in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Emit an OSC 52 sequence for `text` to stdout.
///
/// Wraps in a DCS passthrough when inside tmux. Returns an error when the
/// encoded payload exceeds [`OSC52_MAX`].
pub(crate) fn emit_osc52(text: &str, in_tmux: bool) -> io::Result<()> {
    let encoded = base64_encode(text.as_bytes());
    if encoded.len() > OSC52_MAX {
        return Err(io::Error::other("payload exceeds OSC 52 size cap"));
    }
    let mut out = io::stdout().lock();
    if in_tmux {
        // DCS passthrough: tmux strips the leading ESC from the inner
        // sequence, so we double it. ST (`\e\\`) terminates the DCS.
        write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")?;
    } else {
        write!(out, "\x1b]52;c;{encoded}\x07")?;
    }
    out.flush()
}
