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
/// Used by `Clipboard::new()` for future OSC 52 auto-detect (v0.5).
#[allow(dead_code)]
pub(crate) fn is_over_ssh() -> bool {
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
}

/// Returns `true` when the process is running inside tmux.
pub(crate) fn is_in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Write an OSC 52 sequence for `text` to any [`Write`] sink.
///
/// Wraps in a DCS passthrough when `in_tmux` is true. Returns an error when
/// the encoded payload exceeds [`OSC52_MAX`].
pub(crate) fn write_osc52(out: &mut impl Write, text: &str, in_tmux: bool) -> io::Result<()> {
    let encoded = base64_encode(text.as_bytes());
    if encoded.len() > OSC52_MAX {
        return Err(io::Error::other("payload exceeds OSC 52 size cap"));
    }
    if in_tmux {
        // DCS passthrough: tmux strips the leading ESC from the inner
        // sequence, so we double it. ST (`\e\\`) terminates the DCS.
        write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")?;
    } else {
        write!(out, "\x1b]52;c;{encoded}\x07")?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Tiny inline base64 decoder for test assertions.
    // ---------------------------------------------------------------------------

    fn base64_decode(s: &str) -> Vec<u8> {
        const ALPHA: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| -> u8 {
            ALPHA
                .iter()
                .position(|&a| a == c)
                .expect("invalid base64 char") as u8
        };
        let s = s.trim_end_matches('=');
        let mut out = Vec::with_capacity(s.len() * 3 / 4);
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 3 < bytes.len() {
            let b = (val(bytes[i]) as u32) << 18
                | (val(bytes[i + 1]) as u32) << 12
                | (val(bytes[i + 2]) as u32) << 6
                | (val(bytes[i + 3]) as u32);
            out.push((b >> 16) as u8);
            out.push((b >> 8) as u8);
            out.push(b as u8);
            i += 4;
        }
        let rem = bytes.len() - i;
        if rem == 2 {
            // 2 input chars → 1 output byte
            let b = (val(bytes[i]) as u32) << 18 | (val(bytes[i + 1]) as u32) << 12;
            out.push((b >> 16) as u8);
        } else if rem == 3 {
            // 3 input chars → 2 output bytes
            let b = (val(bytes[i]) as u32) << 18
                | (val(bytes[i + 1]) as u32) << 12
                | (val(bytes[i + 2]) as u32) << 6;
            out.push((b >> 16) as u8);
            out.push((b >> 8) as u8);
        }
        out
    }

    // ---------------------------------------------------------------------------
    // Helper: extract the base64 body from a captured non-tmux OSC 52 sequence.
    // Format: ESC ] 52 ; c ; <base64> BEL
    // ---------------------------------------------------------------------------

    fn extract_body_plain(buf: &[u8]) -> &str {
        let s = std::str::from_utf8(buf).expect("not utf8");
        // strip "\x1b]52;c;" prefix and "\x07" suffix
        let s = s.strip_prefix("\x1b]52;c;").expect("missing OSC prefix");
        s.strip_suffix('\x07').expect("missing BEL suffix")
    }

    // ---------------------------------------------------------------------------
    // Helper: extract the base64 body from a tmux DCS-wrapped OSC 52 sequence.
    // Format: ESC P tmux ; ESC ESC ] 52 ; c ; <base64> BEL ESC \
    // ---------------------------------------------------------------------------

    fn extract_body_tmux(buf: &[u8]) -> &str {
        let s = std::str::from_utf8(buf).expect("not utf8");
        let s = s
            .strip_prefix("\x1bPtmux;\x1b\x1b]52;c;")
            .expect("missing DCS prefix");
        s.strip_suffix("\x07\x1b\\").expect("missing DCS suffix")
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn non_tmux_known_payload() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "hello", false).unwrap();

        let body = extract_body_plain(&buf);
        let decoded = base64_decode(body);
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn tmux_known_payload() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "hello", true).unwrap();

        let body = extract_body_tmux(&buf);
        let decoded = base64_decode(body);
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn non_tmux_empty_payload() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "", false).unwrap();

        let body = extract_body_plain(&buf);
        assert_eq!(body, ""); // empty base64
    }

    #[test]
    fn tmux_empty_payload() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "", true).unwrap();

        let body = extract_body_tmux(&buf);
        assert_eq!(body, "");
    }

    #[test]
    fn oversize_payload_returns_error() {
        // ~55 501 bytes encodes to ~74 002 base64 chars — just over OSC52_MAX.
        let big = "x".repeat(55_501);
        let mut buf = Vec::new();
        let err = write_osc52(&mut buf, &big, false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(
            buf.is_empty(),
            "nothing should be written on oversize error"
        );
    }

    #[test]
    fn non_ascii_and_non_printable_round_trip() {
        // Use valid UTF-8 with control chars, multi-byte sequences, and the
        // replacement character U+FFFD. Rust string literals require valid
        // UTF-8, so we express high bytes via Unicode escape or literal chars.
        // U+0080 and U+0081 are C1 control chars; U+FFFD is replacement char.
        let payload = "\x00\x01\x02\x7f\u{0080}\u{0081}\u{FFFD}";
        let mut buf = Vec::new();
        write_osc52(&mut buf, payload, false).unwrap();

        let body = extract_body_plain(&buf);
        let decoded = base64_decode(body);
        assert_eq!(decoded, payload.as_bytes());
    }

    #[test]
    fn wire_prefix_suffix_non_tmux() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "test", false).unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.starts_with("\x1b]52;c;"), "wrong OSC prefix");
        assert!(s.ends_with('\x07'), "wrong BEL suffix");
    }

    #[test]
    fn wire_prefix_suffix_tmux() {
        let mut buf = Vec::new();
        write_osc52(&mut buf, "test", true).unwrap();
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(
            s.starts_with("\x1bPtmux;\x1b\x1b]52;c;"),
            "wrong DCS prefix"
        );
        assert!(s.ends_with("\x07\x1b\\"), "wrong DCS suffix");
    }
}
