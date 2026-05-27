//! `HexColorPass` — inline color-literal preview overlay.
//!
//! Scans the source for color literals and emits a [`HighlightSpan`] per match
//! with metadata carrying the resolved background colour and a contrasting
//! foreground. Downstream renderers special-case the `hex_color` capture:
//! instead of resolving it through the active theme, they read `hex.bg` /
//! `hex.fg` from `span.metadata` and paint the span with those exact RGB
//! values.
//!
//! Supported formats:
//! - `#rgb` / `#rrggbb` — hex literals.
//! - `rgb(R, G, B)` / `rgba(R, G, B, A)` — integer 0–255 or `0–100%`.
//! - `hsl(H, S%, L%)` / `hsla(H, S%, L%, A)` — H in 0–360°, S/L in %.
//! - Named CSS Level 3 colors (`red`, `tomato`, `rebeccapurple`, etc.).
//!
//! Boundary rules (hex): a hex literal starts on `#` followed by exactly 3 or
//! 6 hex digits. The char immediately before `#` must not be a hex digit or
//! `#` itself. The char immediately after the digit run must not be a hex digit
//! or `_`.
//!
//! Boundary rules (named): only tokens of length 3..=20 that start with
//! `[a-zA-Z]` and are bounded by non-alphanumeric, non-`_` chars on both
//! sides are tested against the named-color table.
//!
//! Contrast policy: relative-luminance (per WCAG) → choose `#000000` when bg
//! luminance > 0.5, else `#ffffff`.

use std::ops::Range;

use crate::HighlightSpan;
use crate::predicate::MetaValue;

/// Capture name used for hex color preview spans. Renderers that
/// understand inline color overlays match on this exact string.
pub const HEX_COLOR_CAPTURE: &str = "hex_color";

/// Metadata key for the rendered background colour (always a full
/// `"#rrggbb"` string regardless of input format).
pub const HEX_BG_KEY: &str = "hex.bg";

/// Metadata key for the contrasting foreground colour, always
/// `"#000000"` or `"#ffffff"`.
pub const HEX_FG_KEY: &str = "hex.fg";

/// Inline color-literal preview overlay pass.
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

    /// Scan `bytes` for color literals and append a [`HighlightSpan`] per
    /// match onto `spans`. Existing spans are not modified — renderers handle
    /// the colour-override at paint time by inspecting `span.metadata`.
    pub fn apply(&self, spans: &mut Vec<HighlightSpan>, bytes: &[u8]) {
        self.apply_range(spans, bytes, 0..bytes.len());
    }

    /// Same as [`Self::apply`] but only scans `range` of `bytes`. Used
    /// by the sync `query_viewport` path so each keystroke only walks
    /// the visible viewport (~80 rows) instead of the full document
    /// (~400 rows on a 400-line file → 5× wasted work per char).
    pub fn apply_range(
        &self,
        spans: &mut Vec<HighlightSpan>,
        bytes: &[u8],
        range: std::ops::Range<usize>,
    ) {
        let start = range.start.min(bytes.len());
        let end = range.end.min(bytes.len()).max(start);
        let slice = &bytes[start..end];
        for (hit, rgb) in scan_color_literals(slice) {
            let abs_hit = (hit.start + start)..(hit.end + start);
            let bg_hex = rgb_to_hex(rgb);
            let fg_hex = contrasting_fg_rgb(rgb);
            let mut span = HighlightSpan {
                byte_range: abs_hit,
                capture: HEX_COLOR_CAPTURE.to_string(),
                metadata: std::collections::HashMap::new(),
            };
            span.metadata
                .insert(HEX_BG_KEY.to_string(), MetaValue::Str(bg_hex));
            span.metadata
                .insert(HEX_FG_KEY.to_string(), MetaValue::Str(fg_hex.to_string()));
            spans.push(span);
        }
    }

    /// Like [`apply_range`] but reads source from a `ropey::Rope`. Only the
    /// requested `range` plus one boundary byte on each side are materialised.
    pub fn apply_range_rope(
        &self,
        spans: &mut Vec<HighlightSpan>,
        rope: &ropey::Rope,
        range: Range<usize>,
    ) {
        let rope_len = rope.len_bytes();
        // Include one byte on each side so left/right boundary checks work.
        let win_start = range.start.saturating_sub(1);
        let win_end = (range.end + 1).min(rope_len);
        if win_start >= win_end {
            return;
        }
        let window_str: String = rope.byte_slice(win_start..win_end).to_string();
        let window: &[u8] = window_str.as_bytes();
        // The scan range within the window, clamped to the window.
        let scan_start = range.start - win_start;
        let scan_end = (range.end - win_start).min(window.len());
        let scan_slice = &window[..scan_end];

        for (hit, rgb) in scan_color_literals(scan_slice) {
            // Only emit hits that fall within the requested range (i.e. start
            // at or after scan_start — left-boundary hits from the leading byte
            // are still checked but the literal must be at scan_start or later).
            if hit.start < scan_start {
                continue;
            }
            let abs_start = win_start + hit.start;
            let abs_end = win_start + hit.end;
            let bg_hex = rgb_to_hex(rgb);
            let fg_hex = contrasting_fg_rgb(rgb);
            let mut span = HighlightSpan {
                byte_range: abs_start..abs_end,
                capture: HEX_COLOR_CAPTURE.to_string(),
                metadata: std::collections::HashMap::new(),
            };
            span.metadata
                .insert(HEX_BG_KEY.to_string(), MetaValue::Str(bg_hex));
            span.metadata
                .insert(HEX_FG_KEY.to_string(), MetaValue::Str(fg_hex.to_string()));
            spans.push(span);
        }
    }
}

// ---------------------------------------------------------------------------
// Unified scanner
// ---------------------------------------------------------------------------

/// Scan `bytes` for all supported color literals. Returns `(byte_range, rgb)`
/// pairs where `byte_range` covers the full literal in `bytes`.
fn scan_color_literals(bytes: &[u8]) -> Vec<(Range<usize>, (u8, u8, u8))> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'#' => {
                if let Some((range, rgb)) = try_scan_hex(bytes, i) {
                    i = range.end;
                    out.push((range, rgb));
                } else {
                    i += 1;
                }
            }
            b'r' | b'R' => {
                if let Some((range, rgb)) = try_scan_func(bytes, i, b"rgb(", b"rgba(") {
                    i = range.end;
                    out.push((range, rgb));
                } else {
                    i += 1;
                }
            }
            b'h' | b'H' => {
                if let Some((range, rgb)) = try_scan_func(bytes, i, b"hsl(", b"hsla(") {
                    i = range.end;
                    out.push((range, rgb));
                } else {
                    i += 1;
                }
            }
            c if c.is_ascii_alphabetic() => {
                if let Some((range, rgb)) = try_scan_named(bytes, i) {
                    i = range.end;
                    out.push((range, rgb));
                } else {
                    // Advance past the identifier to avoid redundant char tests.
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Hex literal scanner
// ---------------------------------------------------------------------------

fn try_scan_hex(bytes: &[u8], i: usize) -> Option<(Range<usize>, (u8, u8, u8))> {
    debug_assert_eq!(bytes[i], b'#');
    // Left boundary: previous byte must not be a hex digit or `#`.
    if i > 0 {
        let prev = bytes[i - 1];
        if is_hex_digit(prev) || prev == b'#' {
            return None;
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
        _ => return None,
    };
    // Right boundary: next byte (if any) must not be a hex digit or `_`.
    let after = start + 1 + lit_len;
    if let Some(&nb) = bytes.get(after)
        && (is_hex_digit(nb) || nb == b'_')
    {
        return None;
    }
    let lit = std::str::from_utf8(&bytes[start..after]).ok()?;
    let rgb = parse_hex_rgb(lit)?;
    Some((start..after, rgb))
}

fn is_hex_digit(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b)
}

// ---------------------------------------------------------------------------
// rgb() / rgba() / hsl() / hsla() scanner
// ---------------------------------------------------------------------------

/// Try to match a function-form color literal starting at `i`.
/// `short_prefix` = `b"rgb("`, `long_prefix` = `b"rgba("` (or hsl variants).
fn try_scan_func(
    bytes: &[u8],
    i: usize,
    short_prefix: &[u8],
    long_prefix: &[u8],
) -> Option<(Range<usize>, (u8, u8, u8))> {
    // Must start at a non-identifier boundary.
    if i > 0 && is_ident_char(bytes[i - 1]) {
        return None;
    }

    // Try the longer prefix first (rgba before rgb, hsla before hsl).
    let (prefix_len, is_alpha) =
        if bytes.len() >= i + long_prefix.len() && bytes_iequal(&bytes[i..], long_prefix) {
            (long_prefix.len(), true)
        } else if bytes.len() >= i + short_prefix.len() && bytes_iequal(&bytes[i..], short_prefix) {
            (short_prefix.len(), false)
        } else {
            return None;
        };

    // Find the closing `)`.
    let body_start = i + prefix_len;
    let close = bytes[body_start..].iter().position(|&b| b == b')')?;
    let body_end = body_start + close;
    let body = std::str::from_utf8(&bytes[body_start..body_end]).ok()?;

    // Parse the body into (r, g, b), dropping alpha when present.
    let is_hsl = short_prefix == b"hsl(" || short_prefix == b"HSL(";
    let func_name = &bytes[i..i
        + (if is_alpha {
            long_prefix.len() - 1
        } else {
            short_prefix.len() - 1
        })];
    let is_hsl_variant =
        func_name.eq_ignore_ascii_case(b"hsl") || func_name.eq_ignore_ascii_case(b"hsla");
    let _ = is_hsl; // resolved above

    let rgb = if is_hsl_variant {
        parse_hsl_body(body)?
    } else {
        parse_rgb_body(body)?
    };

    let end = body_end + 1; // include ')'
    Some((i..end, rgb))
}

/// Case-insensitive prefix match for ASCII prefix slices.
fn bytes_iequal(haystack: &[u8], prefix: &[u8]) -> bool {
    if haystack.len() < prefix.len() {
        return false;
    }
    haystack[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Parse the comma-separated body of `rgb(…)` / `rgba(…)` (without the parens).
/// Components may be integers 0–255 or floats 0–100 followed by `%`.
/// Alpha component is silently dropped. Returns `None` for out-of-range values.
pub fn parse_rgb_func(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim();
    // Strip `rgba(` or `rgb(` prefix and trailing `)`.
    let inner = if let Some(rest) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("RGBA(")) {
        rest.strip_suffix(')')?
    } else if let Some(rest) = s.strip_prefix("rgb(").or_else(|| s.strip_prefix("RGB(")) {
        rest.strip_suffix(')')?
    } else {
        // Accept already-stripped body too.
        s
    };
    parse_rgb_body(inner)
}

fn parse_rgb_body(body: &str) -> Option<(u8, u8, u8)> {
    let parts: Vec<&str> = body.split(',').collect();
    if parts.len() < 3 {
        return None;
    }
    let r = parse_color_component(parts[0].trim())?;
    let g = parse_color_component(parts[1].trim())?;
    let b = parse_color_component(parts[2].trim())?;
    Some((r, g, b))
}

/// Parse a single RGB component: integer 0–255 or float 0–100 with `%`.
fn parse_color_component(s: &str) -> Option<u8> {
    if let Some(pct) = s.strip_suffix('%') {
        let f: f32 = pct.trim().parse().ok()?;
        if !(0.0..=100.0).contains(&f) {
            return None;
        }
        Some((f / 100.0 * 255.0).round() as u8)
    } else {
        let n: i32 = s.parse().ok()?;
        if !(0..=255).contains(&n) {
            return None;
        }
        Some(n as u8)
    }
}

// ---------------------------------------------------------------------------
// hsl() / hsla() parser
// ---------------------------------------------------------------------------

/// Parse `"hsl(H, S%, L%)"` or `"hsla(H, S%, L%, A)"`.
/// H is in degrees 0–360 (wrapped), S/L in percent 0–100.
/// Returns `None` for out-of-range components.
pub fn parse_hsl_func(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim();
    let inner = if let Some(rest) = s.strip_prefix("hsla(").or_else(|| s.strip_prefix("HSLA(")) {
        rest.strip_suffix(')')?
    } else if let Some(rest) = s.strip_prefix("hsl(").or_else(|| s.strip_prefix("HSL(")) {
        rest.strip_suffix(')')?
    } else {
        s
    };
    parse_hsl_body(inner)
}

fn parse_hsl_body(body: &str) -> Option<(u8, u8, u8)> {
    let parts: Vec<&str> = body.split(',').collect();
    if parts.len() < 3 {
        return None;
    }
    // H — degrees (may have `deg` suffix or bare float).
    let h_str = parts[0].trim().trim_end_matches("deg").trim();
    let h: f32 = h_str.parse().ok()?;
    // Wrap H to [0, 360).
    let h = ((h % 360.0) + 360.0) % 360.0;

    let s_str = parts[1].trim();
    let s_pct = s_str.strip_suffix('%')?;
    let s: f32 = s_pct.trim().parse().ok()?;
    if !(0.0..=100.0).contains(&s) {
        return None;
    }

    let l_str = parts[2].trim();
    let l_pct = l_str.strip_suffix('%')?;
    let l: f32 = l_pct.trim().parse().ok()?;
    if !(0.0..=100.0).contains(&l) {
        return None;
    }

    Some(hsl_to_rgb(h, s / 100.0, l / 100.0))
}

/// Convert HSL (H in degrees, S/L in 0.0–1.0) to 8-bit RGB.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let r = hue_to_rgb(p, q, h / 360.0 + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h / 360.0);
    let b = hue_to_rgb(p, q, h / 360.0 - 1.0 / 3.0);
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

// ---------------------------------------------------------------------------
// Named color scanner
// ---------------------------------------------------------------------------

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn try_scan_named(bytes: &[u8], i: usize) -> Option<(Range<usize>, (u8, u8, u8))> {
    // Left boundary: must not be preceded by an ident char.
    if i > 0 && is_ident_char(bytes[i - 1]) {
        return None;
    }
    // Quick filter: first byte must be ASCII alpha.
    if !bytes[i].is_ascii_alphabetic() {
        return None;
    }
    // Measure the identifier length (max 20 to skip long tokens cheaply).
    let mut len = 0usize;
    while i + len < bytes.len() && is_ident_char(bytes[i + len]) {
        len += 1;
        if len > 20 {
            return None;
        }
    }
    if len < 3 {
        return None;
    }
    // Right boundary: next byte must not be an ident char.
    if let Some(&nb) = bytes.get(i + len)
        && is_ident_char(nb)
    {
        return None;
    }
    let word = std::str::from_utf8(&bytes[i..i + len]).ok()?;
    let rgb = named_color(word)?;
    Some((i..i + len, rgb))
}

/// Look up a CSS Level 3 named color. Case-insensitive. Returns `None` for
/// unknown names.
pub fn named_color(name: &str) -> Option<(u8, u8, u8)> {
    // Lowercase on the stack (max 20 bytes per identifier filter above).
    let mut buf = [0u8; 20];
    let bytes = name.as_bytes();
    if bytes.len() > 20 {
        return None;
    }
    for (i, &b) in bytes.iter().enumerate() {
        buf[i] = b.to_ascii_lowercase();
    }
    let lower = std::str::from_utf8(&buf[..bytes.len()]).ok()?;
    Some(match lower {
        // CSS Color Level 3 — 147 names (plus transparent).
        "aliceblue" => (240, 248, 255),
        "antiquewhite" => (250, 235, 215),
        "aqua" => (0, 255, 255),
        "aquamarine" => (127, 255, 212),
        "azure" => (240, 255, 255),
        "beige" => (245, 245, 220),
        "bisque" => (255, 228, 196),
        "black" => (0, 0, 0),
        "blanchedalmond" => (255, 235, 205),
        "blue" => (0, 0, 255),
        "blueviolet" => (138, 43, 226),
        "brown" => (165, 42, 42),
        "burlywood" => (222, 184, 135),
        "cadetblue" => (95, 158, 160),
        "chartreuse" => (127, 255, 0),
        "chocolate" => (210, 105, 30),
        "coral" => (255, 127, 80),
        "cornflowerblue" => (100, 149, 237),
        "cornsilk" => (255, 248, 220),
        "crimson" => (220, 20, 60),
        "cyan" => (0, 255, 255),
        "darkblue" => (0, 0, 139),
        "darkcyan" => (0, 139, 139),
        "darkgoldenrod" => (184, 134, 11),
        "darkgray" => (169, 169, 169),
        "darkgreen" => (0, 100, 0),
        "darkgrey" => (169, 169, 169),
        "darkkhaki" => (189, 183, 107),
        "darkmagenta" => (139, 0, 139),
        "darkolivegreen" => (85, 107, 47),
        "darkorange" => (255, 140, 0),
        "darkorchid" => (153, 50, 204),
        "darkred" => (139, 0, 0),
        "darksalmon" => (233, 150, 122),
        "darkseagreen" => (143, 188, 143),
        "darkslateblue" => (72, 61, 139),
        "darkslategray" => (47, 79, 79),
        "darkslategrey" => (47, 79, 79),
        "darkturquoise" => (0, 206, 209),
        "darkviolet" => (148, 0, 211),
        "deeppink" => (255, 20, 147),
        "deepskyblue" => (0, 191, 255),
        "dimgray" => (105, 105, 105),
        "dimgrey" => (105, 105, 105),
        "dodgerblue" => (30, 144, 255),
        "firebrick" => (178, 34, 34),
        "floralwhite" => (255, 250, 240),
        "forestgreen" => (34, 139, 34),
        "fuchsia" => (255, 0, 255),
        "gainsboro" => (220, 220, 220),
        "ghostwhite" => (248, 248, 255),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "gray" => (128, 128, 128),
        "green" => (0, 128, 0),
        "greenyellow" => (173, 255, 47),
        "grey" => (128, 128, 128),
        "honeydew" => (240, 255, 240),
        "hotpink" => (255, 105, 180),
        "indianred" => (205, 92, 92),
        "indigo" => (75, 0, 130),
        "ivory" => (255, 255, 240),
        "khaki" => (240, 230, 140),
        "lavender" => (230, 230, 250),
        "lavenderblush" => (255, 240, 245),
        "lawngreen" => (124, 252, 0),
        "lemonchiffon" => (255, 250, 205),
        "lightblue" => (173, 216, 230),
        "lightcoral" => (240, 128, 128),
        "lightcyan" => (224, 255, 255),
        "lightgoldenrodyellow" => (250, 250, 210),
        "lightgray" => (211, 211, 211),
        "lightgreen" => (144, 238, 144),
        "lightgrey" => (211, 211, 211),
        "lightpink" => (255, 182, 193),
        "lightsalmon" => (255, 160, 122),
        "lightseagreen" => (32, 178, 170),
        "lightskyblue" => (135, 206, 250),
        "lightslategray" => (119, 136, 153),
        "lightslategrey" => (119, 136, 153),
        "lightsteelblue" => (176, 196, 222),
        "lightyellow" => (255, 255, 224),
        "lime" => (0, 255, 0),
        "limegreen" => (50, 205, 50),
        "linen" => (250, 240, 230),
        "magenta" => (255, 0, 255),
        "maroon" => (128, 0, 0),
        "mediumaquamarine" => (102, 205, 170),
        "mediumblue" => (0, 0, 205),
        "mediumorchid" => (186, 85, 211),
        "mediumpurple" => (147, 112, 219),
        "mediumseagreen" => (60, 179, 113),
        "mediumslateblue" => (123, 104, 238),
        "mediumspringgreen" => (0, 250, 154),
        "mediumturquoise" => (72, 209, 204),
        "mediumvioletred" => (199, 21, 133),
        "midnightblue" => (25, 25, 112),
        "mintcream" => (245, 255, 250),
        "mistyrose" => (255, 228, 225),
        "moccasin" => (255, 228, 181),
        "navajowhite" => (255, 222, 173),
        "navy" => (0, 0, 128),
        "oldlace" => (253, 245, 230),
        "olive" => (128, 128, 0),
        "olivedrab" => (107, 142, 35),
        "orange" => (255, 165, 0),
        "orangered" => (255, 69, 0),
        "orchid" => (218, 112, 214),
        "palegoldenrod" => (238, 232, 170),
        "palegreen" => (152, 251, 152),
        "paleturquoise" => (175, 238, 238),
        "palevioletred" => (219, 112, 147),
        "papayawhip" => (255, 239, 213),
        "peachpuff" => (255, 218, 185),
        "peru" => (205, 133, 63),
        "pink" => (255, 192, 203),
        "plum" => (221, 160, 221),
        "powderblue" => (176, 224, 230),
        "purple" => (128, 0, 128),
        "rebeccapurple" => (102, 51, 153),
        "red" => (255, 0, 0),
        "rosybrown" => (188, 143, 143),
        "royalblue" => (65, 105, 225),
        "saddlebrown" => (139, 69, 19),
        "salmon" => (250, 128, 114),
        "sandybrown" => (244, 164, 96),
        "seagreen" => (46, 139, 87),
        "seashell" => (255, 245, 238),
        "sienna" => (160, 82, 45),
        "silver" => (192, 192, 192),
        "skyblue" => (135, 206, 235),
        "slateblue" => (106, 90, 205),
        "slategray" => (112, 128, 144),
        "slategrey" => (112, 128, 144),
        "snow" => (255, 250, 250),
        "springgreen" => (0, 255, 127),
        "steelblue" => (70, 130, 180),
        "tan" => (210, 180, 140),
        "teal" => (0, 128, 128),
        "thistle" => (216, 191, 216),
        "tomato" => (255, 99, 71),
        "turquoise" => (64, 224, 208),
        "violet" => (238, 130, 238),
        "wheat" => (245, 222, 179),
        "white" => (255, 255, 255),
        "whitesmoke" => (245, 245, 245),
        "yellow" => (255, 255, 0),
        "yellowgreen" => (154, 205, 50),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Shared colour helpers
// ---------------------------------------------------------------------------

/// Format an `(r, g, b)` triple as a lowercase `"#rrggbb"` string.
fn rgb_to_hex(rgb: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb.0, rgb.1, rgb.2)
}

/// Compute the contrasting foreground for an RGB triple.
/// Returns `"#000000"` on light backgrounds and `"#ffffff"` on dark.
fn contrasting_fg_rgb(rgb: (u8, u8, u8)) -> &'static str {
    let lum = relative_luminance(rgb.0, rgb.1, rgb.2);
    if lum > 0.5 { "#000000" } else { "#ffffff" }
}

/// Parse a `#rgb` or `#rrggbb` literal into 0–255 RGB.
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

    // -----------------------------------------------------------------------
    // parse_rgb_func
    // -----------------------------------------------------------------------

    #[test]
    fn parse_rgb_basic() {
        assert_eq!(parse_rgb_func("rgb(255, 136, 0)"), Some((255, 136, 0)));
    }

    #[test]
    fn parse_rgb_percent() {
        assert_eq!(parse_rgb_func("rgb(100%, 50%, 0%)"), Some((255, 128, 0)));
    }

    #[test]
    fn parse_rgb_whitespace_tolerated() {
        assert_eq!(parse_rgb_func("rgb( 255 , 136 , 0 )"), Some((255, 136, 0)));
    }

    #[test]
    fn parse_rgb_out_of_range_rejected() {
        assert_eq!(parse_rgb_func("rgb(256, 0, 0)"), None);
        assert_eq!(parse_rgb_func("rgb(0, -1, 0)"), None);
    }

    #[test]
    fn parse_rgba_drops_alpha() {
        assert_eq!(parse_rgb_func("rgba(255, 0, 0, 0.5)"), Some((255, 0, 0)));
    }

    // -----------------------------------------------------------------------
    // parse_hsl_func
    // -----------------------------------------------------------------------

    #[test]
    fn parse_hsl_basic() {
        // hsl(0°, 100%, 50%) → pure red
        let (r, g, b) = parse_hsl_func("hsl(0, 100%, 50%)").unwrap();
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn parse_hsl_blue() {
        // hsl(240°, 100%, 50%) → pure blue
        let (r, g, b) = parse_hsl_func("hsl(240, 100%, 50%)").unwrap();
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 255);
    }

    #[test]
    fn parse_hsla_drops_alpha() {
        let result = parse_hsl_func("hsla(120, 100%, 50%, 0.5)");
        // green
        assert!(result.is_some());
        let (r, g, b) = result.unwrap();
        assert_eq!(r, 0);
        assert_eq!(b, 0);
        assert!(g > 200);
    }

    // -----------------------------------------------------------------------
    // named_color
    // -----------------------------------------------------------------------

    #[test]
    fn named_color_tomato() {
        assert_eq!(named_color("tomato"), Some((255, 99, 71)));
    }

    #[test]
    fn named_color_rebeccapurple() {
        assert_eq!(named_color("rebeccapurple"), Some((102, 51, 153)));
    }

    #[test]
    fn named_color_case_insensitive() {
        assert_eq!(named_color("TOMATO"), Some((255, 99, 71)));
        assert_eq!(named_color("Tomato"), Some((255, 99, 71)));
        assert_eq!(named_color("ToMaTo"), Some((255, 99, 71)));
    }

    #[test]
    fn named_color_unknown_returns_none() {
        assert_eq!(named_color("foobarbaz"), None);
        assert_eq!(named_color("notacolor"), None);
    }

    #[test]
    fn named_color_red_blue_black_white() {
        assert_eq!(named_color("red"), Some((255, 0, 0)));
        assert_eq!(named_color("blue"), Some((0, 0, 255)));
        assert_eq!(named_color("black"), Some((0, 0, 0)));
        assert_eq!(named_color("white"), Some((255, 255, 255)));
    }

    // -----------------------------------------------------------------------
    // Scan integration
    // -----------------------------------------------------------------------

    #[test]
    fn scan_emits_span_for_rgb() {
        let src = b"color: rgb(255, 0, 0);";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1, "expected one span");
        assert_eq!(spans[0].capture, HEX_COLOR_CAPTURE);
        assert_eq!(
            spans[0].metadata.get(HEX_BG_KEY),
            Some(&MetaValue::Str("#ff0000".into()))
        );
    }

    #[test]
    fn scan_emits_span_for_rgba() {
        let src = b"background: rgba(0, 128, 255, 0.8);";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].metadata.get(HEX_BG_KEY),
            Some(&MetaValue::Str("#0080ff".into()))
        );
    }

    #[test]
    fn scan_emits_span_for_hsl() {
        let src = b"color: hsl(0, 100%, 50%);";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].metadata.get(HEX_BG_KEY),
            Some(&MetaValue::Str("#ff0000".into()))
        );
    }

    #[test]
    fn scan_emits_span_for_named() {
        let src = b"color: tomato;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1, "expected span for named color 'tomato'");
        assert_eq!(
            spans[0].metadata.get(HEX_BG_KEY),
            Some(&MetaValue::Str("#ff6347".into()))
        );
    }

    #[test]
    fn scan_skips_named_inside_identifier() {
        // "myred" — "red" is embedded in an identifier; must not match.
        let src = b"let myred = 1;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert!(spans.is_empty(), "should not match embedded identifier");
    }

    // -----------------------------------------------------------------------
    // Legacy hex tests (preserved)
    // -----------------------------------------------------------------------

    #[test]
    fn scans_six_digit_hex() {
        let src = b"color: #bb9af7;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].byte_range, 7..14);
    }

    #[test]
    fn scans_three_digit_hex() {
        let src = b"color: #abc;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].byte_range, 7..11);
    }

    #[test]
    fn scans_at_start_of_line() {
        let src = b"#fff";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].byte_range, 0..4);
    }

    #[test]
    fn scans_multiple_per_line() {
        let src = b"--bg: #0b0d10; --fg: #e5e9f0;";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].byte_range, 6..13);
        assert_eq!(spans[1].byte_range, 21..28);
    }

    #[test]
    fn rejects_seven_or_eight_digit_runs() {
        let src = b"#bb9af7ff #aabbccd";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        // Must not match 8-digit or 7-digit hex runs.
        // May match named colors in the fragment — but there are none here.
        let hex_spans: Vec<_> = spans
            .iter()
            .filter(|s| {
                s.metadata
                    .get(HEX_BG_KEY)
                    .and_then(|v| {
                        if let MetaValue::Str(s) = v {
                            Some(s.as_str())
                        } else {
                            None
                        }
                    })
                    .map(|bg| bg.starts_with('#') && bg.len() == 7)
                    .unwrap_or(false)
            })
            .collect();
        // None of the hex spans should be from the 8/7-digit runs.
        for s in &hex_spans {
            assert!(
                s.byte_range.start != 0 || s.byte_range.end <= 8,
                "should not have matched 8-digit hex"
            );
        }
    }

    #[test]
    fn rejects_identifier_fragments() {
        // `#abc_def` — `_` extends the token; must not match.
        let src = b"#abc_def";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        let hex_spans: Vec<_> = spans.iter().filter(|s| s.byte_range.start == 0).collect();
        assert!(
            hex_spans.is_empty(),
            "rejected identifier-like fragment, got {hex_spans:?}"
        );
    }

    #[test]
    fn rejects_when_previous_char_is_hex() {
        // `123#abc` — `3` before `#` is a hex digit; must not match.
        let src = b"123#abc";
        let mut spans: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply(&mut spans, src);
        let hex_spans: Vec<_> = spans.iter().filter(|s| s.byte_range.start == 3).collect();
        assert!(
            hex_spans.is_empty(),
            "rejected when preceding char is hex digit, got {hex_spans:?}"
        );
    }

    #[test]
    fn contrast_dark_bg_returns_white_fg() {
        assert_eq!(contrasting_fg_rgb((0, 0, 0)), "#ffffff");
        assert_eq!(contrasting_fg_rgb((11, 13, 16)), "#ffffff");
    }

    #[test]
    fn contrast_light_bg_returns_black_fg() {
        assert_eq!(contrasting_fg_rgb((255, 255, 255)), "#000000");
        assert_eq!(contrasting_fg_rgb((229, 233, 240)), "#000000");
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

    /// Smoke: `apply_range_rope` produces identical spans to `apply_range`
    /// on a small input — no grammar required.
    #[test]
    fn apply_range_rope_matches_bytes_variant() {
        let src = "--accent: #bb9af7; --bg: #0b0d10;";
        let bytes = src.as_bytes();
        let rope = ropey::Rope::from_str(src);
        let range = 0..bytes.len();

        let mut spans_bytes: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply_range(&mut spans_bytes, bytes, range.clone());

        let mut spans_rope: Vec<HighlightSpan> = Vec::new();
        HexColorPass::new().apply_range_rope(&mut spans_rope, &rope, range);

        assert_eq!(
            spans_bytes.len(),
            spans_rope.len(),
            "rope and bytes variants must emit same number of spans"
        );
        for (b, r) in spans_bytes.iter().zip(spans_rope.iter()) {
            assert_eq!(b.byte_range, r.byte_range, "byte_range mismatch");
            assert_eq!(b.capture, r.capture, "capture mismatch");
            assert_eq!(
                b.metadata.get(HEX_BG_KEY),
                r.metadata.get(HEX_BG_KEY),
                "HEX_BG_KEY mismatch"
            );
        }
    }
}
