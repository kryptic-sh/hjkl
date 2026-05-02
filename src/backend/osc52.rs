//! OSC 52 clipboard backend — write-only SSH/terminal fallback.
//!
//! Only `MimeType::Text` and `Selection::Clipboard` are supported.
//! `Selection::Primary` returns `ClipboardError::UnsupportedMime` because OSC
//! 52 is a single-channel protocol; there is no standard way to target the
//! primary selection via the escape sequence. `get` is unsupported (OSC 52 is
//! write-only from the application side). `available` returns an empty list
//! because we cannot query the terminal's clipboard contents.

use std::io::{self, Write};

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;
use crate::osc52::{OSC52_MAX, is_in_tmux, write_osc52};

/// Length of the standard padded base64 encoding of `n` raw bytes.
///
/// Standard base64 emits `((n + 2) / 3) * 4` characters (every 3-byte input
/// chunk produces 4 output characters; partial trailing chunks are padded with
/// `=` to a 4-character boundary). Computed without allocating so the OSC 52
/// size cap can be enforced on huge payloads cheaply.
#[inline]
fn base64_encoded_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

/// OSC 52 backend. Unit struct — no state, everything is stateless I/O.
pub(crate) struct Osc52Backend;

impl Osc52Backend {
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    pub(crate) fn new() -> Self {
        Self
    }

    /// Inner set implementation that writes to an arbitrary `Write` sink.
    ///
    /// Extracted so tests can capture output without touching stdout.
    pub(crate) fn set_inner(
        &self,
        sel: Selection,
        mime: MimeType,
        bytes: &[u8],
        out: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        match mime {
            MimeType::Text => {}
            _ => return Err(ClipboardError::UnsupportedMime),
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ClipboardError::UnsupportedMime)?;
        // Check size cap before writing — avoids relying on error-kind heuristics.
        // Compute the encoded length arithmetically rather than allocating a
        // base64 string just to measure it; the formula matches the encoder.
        if base64_encoded_len(text.len()) > OSC52_MAX {
            return Err(ClipboardError::PayloadTooLarge);
        }
        write_osc52(out, text, is_in_tmux()).map_err(ClipboardError::io)
    }

    /// Inner clear implementation that writes to an arbitrary `Write` sink.
    pub(crate) fn clear_inner(
        &self,
        sel: Selection,
        out: &mut impl Write,
    ) -> Result<(), ClipboardError> {
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        // Empty base64 payload tells the terminal to clear its clipboard.
        write_osc52(out, "", is_in_tmux()).map_err(ClipboardError::io)
    }
}

impl Backend for Osc52Backend {
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        self.set_inner(sel, mime, bytes, &mut io::stdout().lock())
    }

    /// OSC 52 is write-only. Reading is not possible.
    fn get(&self, _sel: Selection, _mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        Err(ClipboardError::UnsupportedMime)
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        self.clear_inner(sel, &mut io::stdout().lock())
    }

    /// Cannot query what's in the terminal clipboard. Returns empty list.
    fn available(&self, _sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base64::base64_encode;

    fn backend() -> Osc52Backend {
        Osc52Backend::new()
    }

    // -------------------------------------------------------------------------
    // base64_encoded_len — must match `base64_encode(...).len()` exactly.
    // -------------------------------------------------------------------------

    /// The arithmetic length must match what the encoder actually produces for
    /// boundary inputs (0/1/2/3-byte remainders) and at the OSC 52 cap edge.
    #[test]
    fn base64_encoded_len_matches_encoder() {
        // 0/1/2/3 cover every chunks_exact remainder branch (0, 1, 2, none).
        for n in [0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
            let bytes = vec![b'x'; n];
            assert_eq!(
                base64_encoded_len(n),
                base64_encode(&bytes).len(),
                "mismatch at n = {n}"
            );
        }

        // OSC52_MAX boundary inputs — sizes that bracket the cap on either
        // side (just under, exactly at, just over the encoded-length limit).
        // These exercise the same arithmetic the cap-check uses.
        let max_raw = OSC52_MAX / 4 * 3; // largest multiple-of-3 raw input that fits
        for &n in &[
            max_raw.saturating_sub(2),
            max_raw.saturating_sub(1),
            max_raw,
            max_raw + 1,
            max_raw + 2,
        ] {
            let bytes = vec![b'x'; n];
            assert_eq!(
                base64_encoded_len(n),
                base64_encode(&bytes).len(),
                "mismatch at n = {n}"
            );
        }
    }

    // -------------------------------------------------------------------------
    // set_inner — text + clipboard: should succeed and write something.
    // -------------------------------------------------------------------------

    #[test]
    fn set_text_clipboard_ok() {
        let b = backend();
        let mut buf = Vec::new();
        let result = b.set_inner(Selection::Clipboard, MimeType::Text, b"hello", &mut buf);
        assert!(result.is_ok());
        assert!(!buf.is_empty(), "expected bytes written to sink");
    }

    // -------------------------------------------------------------------------
    // set_inner — unsupported mimes.
    // -------------------------------------------------------------------------

    #[test]
    fn set_html_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(Selection::Clipboard, MimeType::Html, b"<b>hi</b>", &mut buf)
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn set_rtf_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(Selection::Clipboard, MimeType::Rtf, b"{\\rtf1}", &mut buf)
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn set_png_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(Selection::Clipboard, MimeType::Png, b"\x89PNG", &mut buf)
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn set_uri_list_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(
                Selection::Clipboard,
                MimeType::UriList,
                b"file:///tmp/x",
                &mut buf,
            )
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn set_custom_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(
                Selection::Clipboard,
                MimeType::Custom("application/json".into()),
                b"{}",
                &mut buf,
            )
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    // -------------------------------------------------------------------------
    // set_inner — invalid UTF-8.
    // -------------------------------------------------------------------------

    #[test]
    fn set_non_utf8_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let invalid_utf8 = b"\xff\xfe";
        let err = b
            .set_inner(Selection::Clipboard, MimeType::Text, invalid_utf8, &mut buf)
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    // -------------------------------------------------------------------------
    // set_inner — Primary selection rejected.
    // -------------------------------------------------------------------------

    #[test]
    fn set_primary_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(Selection::Primary, MimeType::Text, b"hi", &mut buf)
            .unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    // -------------------------------------------------------------------------
    // get — always unsupported.
    // -------------------------------------------------------------------------

    #[test]
    fn get_clipboard_text_unsupported() {
        let b = backend();
        let err = b.get(Selection::Clipboard, MimeType::Text).unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    #[test]
    fn get_primary_unsupported() {
        let b = backend();
        let err = b.get(Selection::Primary, MimeType::Html).unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    // -------------------------------------------------------------------------
    // clear_inner.
    // -------------------------------------------------------------------------

    #[test]
    fn clear_clipboard_ok() {
        let b = backend();
        let mut buf = Vec::new();
        let result = b.clear_inner(Selection::Clipboard, &mut buf);
        assert!(result.is_ok());
        // Empty OSC 52 sequence should still produce bytes (the framing).
        assert!(!buf.is_empty());
    }

    #[test]
    fn clear_primary_unsupported() {
        let b = backend();
        let mut buf = Vec::new();
        let err = b.clear_inner(Selection::Primary, &mut buf).unwrap_err();
        assert!(matches!(err, ClipboardError::UnsupportedMime));
    }

    // -------------------------------------------------------------------------
    // set_inner — PayloadTooLarge for oversized payload.
    // -------------------------------------------------------------------------

    #[test]
    fn set_text_over_cap_returns_payload_too_large() {
        // ~55 501 bytes encodes to ~74 002 base64 chars — just over OSC52_MAX.
        let big = "x".repeat(55_501);
        let b = backend();
        let mut buf = Vec::new();
        let err = b
            .set_inner(
                Selection::Clipboard,
                MimeType::Text,
                big.as_bytes(),
                &mut buf,
            )
            .unwrap_err();
        assert!(
            matches!(err, ClipboardError::PayloadTooLarge),
            "expected PayloadTooLarge, got: {err:?}"
        );
        assert!(
            buf.is_empty(),
            "nothing should be written for oversized payload"
        );
    }

    // -------------------------------------------------------------------------
    // available — always empty.
    // -------------------------------------------------------------------------

    #[test]
    fn available_returns_empty() {
        let b = backend();
        let mimes = b.available(Selection::Clipboard).unwrap();
        assert!(mimes.is_empty());
    }

    #[test]
    fn available_primary_returns_empty() {
        let b = backend();
        let mimes = b.available(Selection::Primary).unwrap();
        assert!(mimes.is_empty());
    }
}
