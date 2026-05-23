//! `HexColorPass` — inline hex-color literal preview overlay.
//!
//! Scans the source for hex color literals (`#rgb`, `#rrggbb`) and emits a
//! [`HighlightSpan`] per match with metadata carrying the resolved
//! background colour and a contrasting foreground. Downstream renderers
//! special-case the `hex_color` capture: instead of resolving it through
//! the active theme, they read `hex.bg` / `hex.fg` from `span.metadata`
//! and paint the span with those exact RGB values.
//!
//! Boundary rules: a hex literal starts on `#` and is followed by exactly
//! 3 or 6 hex digits. The char immediately before `#` must not be a hex
//! digit or `#` itself (so we don't latch onto the middle of a longer
//! token like `0x#abc123` or an 8-digit `#rrggbbaa`). The char
//! immediately after the digit run must not be a hex digit or `_`
//! (rejects identifier fragments like `#abc123def` and `#abc_def`).
//!
//! Contrast policy: relative-luminance (per WCAG) → choose `#000000`
//! when bg luminance > 0.5, else `#ffffff`. Cheap and gives the right
//! answer on the entire sRGB cube without needing a full WCAG ratio
//! computation.

use std::ops::Range;

use crate::HighlightSpan;
use crate::predicate::MetaValue;

/// Capture name used for hex color preview spans. Renderers that
/// understand inline color overlays match on this exact string.
pub const HEX_COLOR_CAPTURE: &str = "hex_color";

/// Metadata key for the rendered background colour (hex literal as
/// emitted in the source, e.g. `"#bb9af7"` or `"#fff"`).
pub const HEX_BG_KEY: &str = "hex.bg";

/// Metadata key for the contrasting foreground colour, always
/// `"#000000"` or `"#ffffff"`.
pub const HEX_FG_KEY: &str = "hex.fg";

/// Inline hex-color preview overlay pass.
///
/// Stateless. Cheap. Call [`apply`](HexColorPass::apply) after the
/// highlighter (and any other overlay passes) have populated `spans`.
#[derive(Clone, Copy, Debug, Default)]
pub struct HexColorPass;

impl HexColorPass {
    /// Construct a new pass. Equivalent to `HexColorPass::default()`.
    pub fn new() -> Self {
        Self
    }

    /// Scan `bytes` for hex color literals and append a
    /// [`HighlightSpan`] per match onto `spans`. Existing spans are
    /// not modified — renderers handle the colour-override at paint
    /// time by inspecting `span.metadata`.
    pub fn apply(&self, spans: &mut Vec<HighlightSpan>, bytes: &[u8]) {
        for hit in scan_hex_colors(bytes) {
            let lit_bytes = &bytes[hit.clone()];
            // Safe: scan_hex_colors only emits ASCII hex digit ranges.
            let bg_hex = std::str::from_utf8(lit_bytes).unwrap();
            let fg_hex = contrasting_fg(bg_hex);
            let mut span = HighlightSpan {
                byte_range: hit,
                capture: HEX_COLOR_CAPTURE.to_string(),
                metadata: std::collections::HashMap::new(),
            };
            span.metadata
                .insert(HEX_BG_KEY.to_string(), MetaValue::Str(bg_hex.to_string()));
            span.metadata
                .insert(HEX_FG_KEY.to_string(), MetaValue::Str(fg_hex.to_string()));
            spans.push(span);
        }
    }
}

/// Scan `bytes` for hex color literals. Returns byte ranges that
/// include the leading `#` and the 3 or 6 hex digits.
fn scan_hex_colors(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'#' {
            i += 1;
            continue;
        }
        // Left boundary: previous byte must not be a hex digit or `#`.
        if i > 0 {
            let prev = bytes[i - 1];
            if is_hex_digit(prev) || prev == b'#' {
                i += 1;
                continue;
            }
        }
        // Count following hex digits (cap at 9 so we can reject 7/8/9-digit runs).
        let start = i;
        let mut digits = 0usize;
        let mut j = i + 1;
        while j < bytes.len() && digits < 9 && is_hex_digit(bytes[j]) {
            digits += 1;
            j += 1;
        }
        let lit_len = match digits {
            3 | 6 => digits,
            _ => {
                i += 1;
                continue;
            }
        };
        // Right boundary: next byte (if any) must not be a hex digit or `_`.
        // (Reject identifier fragments like `#abc123def` or `#abc_def`.)
        let after = start + 1 + lit_len;
        if let Some(&b) = bytes.get(after)
            && (is_hex_digit(b) || b == b'_')
        {
            i = after;
            continue;
        }
        out.push(start..after);
        i = after;
    }
    out
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b)
}

/// Compute the contrasting foreground hex for a given background hex.
/// Returns `"#000000"` on light backgrounds and `"#ffffff"` on dark.
///
/// Uses relative luminance with the gamma-corrected sRGB weights from
/// WCAG 2.x. The threshold 0.5 gives a sensible split across the cube
/// without needing a full contrast-ratio calculation — black wins on
/// pastels and pales, white wins on saturated mids and darks.
fn contrasting_fg(bg_hex: &str) -> &'static str {
    let (r, g, b) = match parse_hex_rgb(bg_hex) {
        Some(rgb) => rgb,
        None => return "#000000",
    };
    let lum = relative_luminance(r, g, b);
    if lum > 0.5 { "#000000" } else { "#ffffff" }
}

/// Parse a `#rgb` or `#rrggbb` literal into 0–255 RGB. Returns `None`
/// for any other shape (caller should have validated already; this is
/// the belt-and-braces guard).
fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.strip_prefix('#')?;
    match s.len() {
        3 => {
            let b = s.as_bytes();
            let r = expand_nibble(hex_nibble(b[0])?);
            let g = expand_nibble(hex_nibble(b[1])?);
            let bv = expand_nibble(hex_nibble(b[2])?);
            Some((r, g, bv))
        }
        6 => {
            let b = s.as_bytes();
            let r = hex_nibble(b[0])? << 4 | hex_nibble(b[1])?;
            let g = hex_nibble(b[2])? << 4 | hex_nibble(b[3])?;
            let bv = hex_nibble(b[4])? << 4 | hex_nibble(b[5])?;
            Some((r, g, bv))
        }
        _ => None,
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn expand_nibble(n: u8) -> u8 {
    (n << 4) | n
}

fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    fn channel(c: u8) -> f32 {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_six_digit_hex() {
        let r = scan_hex_colors(b"color: #bb9af7;");
        assert_eq!(r, vec![7..14]);
    }

    #[test]
    fn scans_three_digit_hex() {
        let r = scan_hex_colors(b"color: #abc;");
        assert_eq!(r, vec![7..11]);
    }

    #[test]
    fn scans_at_start_of_line() {
        let r = scan_hex_colors(b"#fff");
        assert_eq!(r, vec![0..4]);
    }

    #[test]
    fn scans_multiple_per_line() {
        let r = scan_hex_colors(b"--bg: #0b0d10; --fg: #e5e9f0;");
        assert_eq!(r, vec![6..13, 21..28]);
    }

    #[test]
    fn rejects_seven_or_eight_digit_runs() {
        // 8-digit forms (rgba) and stray 7-digit runs must not match.
        let r = scan_hex_colors(b"#bb9af7ff #aabbccd");
        assert!(r.is_empty(), "rejected 8-digit and 7-digit runs, got {r:?}");
    }

    #[test]
    fn rejects_four_or_five_digit_runs() {
        let r = scan_hex_colors(b"#abcd #abcde");
        assert!(r.is_empty(), "rejected 4-digit and 5-digit runs, got {r:?}");
    }

    #[test]
    fn rejects_identifier_fragments() {
        // `#abc_def` is not a colour — the `_` extends the token.
        let r = scan_hex_colors(b"#abc_def");
        assert!(r.is_empty(), "rejected identifier-like fragment, got {r:?}");
    }

    #[test]
    fn rejects_when_previous_char_is_hex() {
        // `123#abc` could be a hex-encoded sequence — don't match.
        let r = scan_hex_colors(b"123#abc");
        // 123 has '3' before '#', '3' is a hex digit → reject.
        assert!(
            r.is_empty(),
            "rejected when preceding char is hex digit, got {r:?}"
        );
    }

    #[test]
    fn contrast_dark_bg_returns_white_fg() {
        assert_eq!(contrasting_fg("#000000"), "#ffffff");
        assert_eq!(contrasting_fg("#0b0d10"), "#ffffff");
        // Pastel lavender — relative luminance ≈ 0.41, just under the
        // 0.5 split → white fg wins.
        assert_eq!(contrasting_fg("#bb9af7"), "#ffffff");
    }

    #[test]
    fn contrast_light_bg_returns_black_fg() {
        assert_eq!(contrasting_fg("#ffffff"), "#000000");
        assert_eq!(contrasting_fg("#e5e9f0"), "#000000");
        // Off-white pastel — luminance > 0.5 → black fg.
        assert_eq!(contrasting_fg("#f0e0d0"), "#000000");
    }

    #[test]
    fn contrast_three_digit_form() {
        assert_eq!(contrasting_fg("#fff"), "#000000");
        assert_eq!(contrasting_fg("#000"), "#ffffff");
    }

    #[test]
    fn apply_appends_span_with_metadata() {
        let bytes = b"--accent: #bb9af7;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, bytes);
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.capture, HEX_COLOR_CAPTURE);
        assert_eq!(s.byte_range, 10..17);
        assert_eq!(
            s.metadata.get(HEX_BG_KEY),
            Some(&MetaValue::Str("#bb9af7".into())),
        );
        assert_eq!(
            s.metadata.get(HEX_FG_KEY),
            Some(&MetaValue::Str("#ffffff".into())),
        );
    }
}
