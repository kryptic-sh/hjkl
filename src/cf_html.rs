// Not cfg-gated so pure-Rust tests run on Linux CI; dead_code on non-Windows.
#![allow(dead_code)]

//! Microsoft CF_HTML clipboard format: header wrap and unwrap.
//!
//! CF_HTML is a plain UTF-8 payload with a fixed-shape ASCII header that
//! records byte offsets into the document. This module is **not** cfg-gated
//! so that the pure-Rust `wrap`/`unwrap` functions can be unit-tested on any
//! host platform (including Linux CI).
//!
//! Reference:
//! <https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format>

use crate::ClipboardError;

// ---------------------------------------------------------------------------
// Header template
// ---------------------------------------------------------------------------
//
// The header occupies a fixed number of bytes because every numeric offset is
// zero-padded to exactly 10 digits. We can therefore compute all four offsets
// before emitting the final string.
//
// Template (each line ends with \r\n):
//
//   Version:0.9
//   StartHTML:0000000000
//   EndHTML:0000000000
//   StartFragment:0000000000
//   EndFragment:0000000000
//   SourceURL:about:blank
//
// The exact byte length of `HDR_PREFIX` is verified at runtime by the
// `debug_assert_eq!` in `wrap`. Rust string-literal continuation strips
// leading whitespace, so don't count by eye — trust the assert.
//
// Immediately after the header comes the HTML document:
//
//   <html>\r\n<body>\r\n<!--StartFragment-->…<!--EndFragment-->\r\n</body>\r\n</html>
//
// Structural constants — all lengths in UTF-8 bytes (all ASCII, so 1:1).

const HDR_PREFIX: &str = "Version:0.9\r\n\
                          StartHTML:0000000000\r\n\
                          EndHTML:0000000000\r\n\
                          StartFragment:0000000000\r\n\
                          EndFragment:0000000000\r\n\
                          SourceURL:about:blank\r\n";

// Body wrappers — these surround the caller-supplied fragment.
const BODY_OPEN: &str = "<html>\r\n<body>\r\n<!--StartFragment-->";
const BODY_CLOSE: &str = "<!--EndFragment-->\r\n</body>\r\n</html>";

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Wrap a plain UTF-8 HTML fragment in the CF_HTML envelope.
///
/// The fragment can be any HTML — a full document or just an inline snippet
/// like `<b>hello</b>`. We always emit a complete envelope:
///
/// ```text
/// Version:0.9\r\n
/// StartHTML:NNNNNNNNNN\r\n
/// EndHTML:NNNNNNNNNN\r\n
/// StartFragment:NNNNNNNNNN\r\n
/// EndFragment:NNNNNNNNNN\r\n
/// SourceURL:about:blank\r\n
/// <html>\r\n<body>\r\n<!--StartFragment-->…<!--EndFragment-->\r\n</body>\r\n</html>
/// ```
///
/// Returns the bytes to be passed to `SetClipboardData(CF_HTML, …)`.
pub(crate) fn wrap(html: &str) -> Vec<u8> {
    // Header length is fixed: HDR_PREFIX contains placeholder zeros that are
    // always exactly 10 digits, so its byte length never varies.
    let hdr_len = HDR_PREFIX.len();

    // Byte offset of the opening `<html>` tag.
    let start_html: usize = hdr_len;

    // Byte offset of the `<!--StartFragment-->` marker end (i.e. the first
    // byte of the caller-supplied fragment).
    let start_fragment: usize = hdr_len + BODY_OPEN.len();

    // Byte offset one past the fragment (first byte of `<!--EndFragment-->`).
    let end_fragment: usize = start_fragment + html.len();

    // Byte offset one past `</html>`.
    let end_html: usize = end_fragment + BODY_CLOSE.len();

    // Build the header with the real offsets substituted in.
    let header = format!(
        "Version:0.9\r\n\
         StartHTML:{start_html:010}\r\n\
         EndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\n\
         EndFragment:{end_fragment:010}\r\n\
         SourceURL:about:blank\r\n"
    );

    // Sanity: our header must be the same length as the placeholder template.
    debug_assert_eq!(
        header.len(),
        hdr_len,
        "cf_html: header length changed — update HDR_PREFIX"
    );

    let mut out =
        String::with_capacity(header.len() + BODY_OPEN.len() + html.len() + BODY_CLOSE.len());
    out.push_str(&header);
    out.push_str(BODY_OPEN);
    out.push_str(html);
    out.push_str(BODY_CLOSE);

    out.into_bytes()
}

/// Unwrap a CF_HTML envelope and return the inner fragment.
///
/// Parses the header to find `StartFragment` and `EndFragment` byte offsets,
/// then slices the payload at those positions and validates the result as
/// UTF-8.
///
/// # Errors
///
/// Returns `ClipboardError::Io(other("malformed CF_HTML header"))` if any of
/// the following conditions hold:
///
/// - `StartFragment` or `EndFragment` is missing.
/// - An offset contains non-ASCII-digit characters.
/// - An offset is out of bounds (> payload length).
/// - `StartFragment > EndFragment`.
/// - The sliced fragment is not valid UTF-8.
pub(crate) fn unwrap(payload: &[u8]) -> Result<String, ClipboardError> {
    let bad = || ClipboardError::io_other("malformed CF_HTML header");

    // Parse the header: scan lines until we find all required fields or hit
    // the first `<` character (start of the HTML document).
    let header_text = payload
        .iter()
        .position(|&b| b == b'<')
        .map(|pos| &payload[..pos])
        .unwrap_or(payload);

    let header_str = std::str::from_utf8(header_text).map_err(|_| bad())?;

    let mut start_fragment: Option<usize> = None;
    let mut end_fragment: Option<usize> = None;

    for line in header_str.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(val) = line.strip_prefix("StartFragment:") {
            start_fragment = Some(parse_offset(val).ok_or_else(bad)?);
        } else if let Some(val) = line.strip_prefix("EndFragment:") {
            end_fragment = Some(parse_offset(val).ok_or_else(bad)?);
        }
    }

    let start = start_fragment.ok_or_else(bad)?;
    let end = end_fragment.ok_or_else(bad)?;

    // Validate bounds.
    if start > end {
        return Err(bad());
    }
    if end > payload.len() {
        return Err(bad());
    }

    let fragment_bytes = &payload[start..end];
    let fragment = std::str::from_utf8(fragment_bytes).map_err(|_| bad())?;

    Ok(fragment.to_owned())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse a decimal offset string that must consist entirely of ASCII digits.
/// Returns `None` if any character is not `0`–`9`.
fn parse_offset(s: &str) -> Option<usize> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<usize>().ok()
}

// ---------------------------------------------------------------------------
// Tests — compiled on every host (Linux, macOS, Windows).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build wrap output and assert round-trip.
    fn round_trip(html: &str) {
        let payload = wrap(html);
        let recovered = unwrap(&payload).expect("unwrap failed");
        assert_eq!(recovered, html, "round-trip failed for: {html:?}");
    }

    #[test]
    fn wrap_unwrap_round_trip_plain_text() {
        round_trip("hello");
    }

    #[test]
    fn wrap_unwrap_round_trip_inline_tags() {
        round_trip("<b>hello</b>");
    }

    #[test]
    fn wrap_unwrap_round_trip_multiline() {
        round_trip("<p>one</p>\n<p>two</p>");
    }

    #[test]
    fn wrap_unwrap_round_trip_non_ascii() {
        round_trip("<p>café — naïve</p>");
    }

    #[test]
    fn wrap_unwrap_round_trip_crlf_inside() {
        round_trip("<p>line one\r\nline two</p>");
    }

    #[test]
    fn wrap_unwrap_round_trip_empty() {
        round_trip("");
    }

    #[test]
    fn wrap_produces_valid_utf8() {
        let cases = ["hello", "<b>bold</b>", "<p>café</p>", ""];
        for html in cases {
            let payload = wrap(html);
            assert!(
                std::str::from_utf8(&payload).is_ok(),
                "wrap output is not valid UTF-8 for: {html:?}"
            );
        }
    }

    #[test]
    fn wrap_offsets_are_correct() {
        let html = "<b>test</b>";
        let payload = wrap(html);
        let text = std::str::from_utf8(&payload).unwrap();

        // Parse the four header offsets.
        let start_html = extract_offset(text, "StartHTML:");
        let end_html = extract_offset(text, "EndHTML:");
        let start_frag = extract_offset(text, "StartFragment:");
        let end_frag = extract_offset(text, "EndFragment:");

        // StartHTML offset must point at '<' of "<html>".
        assert_eq!(
            &payload[start_html..start_html + 6],
            b"<html>",
            "StartHTML does not point at <html>"
        );

        // EndHTML offset must be just past "</html>".
        assert_eq!(
            &payload[end_html - 7..end_html],
            b"</html>",
            "EndHTML does not point past </html>"
        );

        // Fragment slice must equal the input html.
        assert_eq!(
            &payload[start_frag..end_frag],
            html.as_bytes(),
            "fragment slice does not match input"
        );
    }

    #[test]
    fn unwrap_rejects_missing_start_fragment() {
        // Build a valid payload then strip StartFragment line.
        let payload = wrap("<p>x</p>");
        let text = String::from_utf8(payload).unwrap();
        let stripped: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("StartFragment:"))
            .map(|l| format!("{l}\r\n"))
            .collect();
        assert!(
            unwrap(stripped.as_bytes()).is_err(),
            "expected error for missing StartFragment"
        );
    }

    #[test]
    fn unwrap_rejects_missing_end_fragment() {
        let payload = wrap("<p>x</p>");
        let text = String::from_utf8(payload).unwrap();
        let stripped: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("EndFragment:"))
            .map(|l| format!("{l}\r\n"))
            .collect();
        assert!(
            unwrap(stripped.as_bytes()).is_err(),
            "expected error for missing EndFragment"
        );
    }

    #[test]
    fn unwrap_rejects_non_numeric_offset() {
        let payload = wrap("<p>x</p>");
        let corrupted = String::from_utf8(payload)
            .unwrap()
            .replace("StartFragment:0000", "StartFragment:XXXX");
        assert!(
            unwrap(corrupted.as_bytes()).is_err(),
            "expected error for non-numeric offset"
        );
    }

    #[test]
    fn unwrap_rejects_offset_out_of_range() {
        let payload = wrap("<p>x</p>");
        let text = String::from_utf8(payload.clone()).unwrap();
        // Replace EndFragment offset with a value larger than payload length.
        let big = format!("{:010}", payload.len() + 9999);
        let corrupted = replace_offset_value(&text, "EndFragment:", &big);
        assert!(
            unwrap(corrupted.as_bytes()).is_err(),
            "expected error for EndFragment out of range"
        );
    }

    #[test]
    fn unwrap_rejects_start_greater_than_end() {
        let payload = wrap("<p>hello world</p>");
        let text = String::from_utf8(payload).unwrap();

        // Extract the real start and end, then swap them.
        let start = extract_offset(&text, "StartFragment:");
        let end = extract_offset(&text, "EndFragment:");

        // Swap: set StartFragment = end, EndFragment = start.
        let swapped = replace_offset_value(
            &replace_offset_value(&text, "StartFragment:", &format!("{end:010}")),
            "EndFragment:",
            &format!("{start:010}"),
        );
        assert!(
            unwrap(swapped.as_bytes()).is_err(),
            "expected error when StartFragment > EndFragment"
        );
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Extract the 10-digit decimal value after `prefix:` from the header.
    fn extract_offset(text: &str, prefix: &str) -> usize {
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(val) = line.strip_prefix(prefix) {
                return val.trim().parse().expect("offset is not a number");
            }
        }
        panic!("offset {prefix:?} not found in header");
    }

    /// Replace the 10-digit value on the line starting with `prefix:`.
    fn replace_offset_value(text: &str, prefix: &str, new_val: &str) -> String {
        text.lines()
            .map(|l| {
                let bare = l.trim_end_matches('\r');
                if bare.starts_with(prefix) {
                    format!("{prefix}{new_val}\r\n")
                } else {
                    format!("{l}\r\n")
                }
            })
            .collect()
    }
}
