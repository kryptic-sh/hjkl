use std::path::Path;

use hjkl_buffer::Span as BufferSpan;
use ratatui::style::Style as RatStyle;

/// Cap preview reads at this many lines so giant files don't stall the
/// render path.
pub const PREVIEW_MAX_LINES: usize = 200;
/// Skip preview entirely past this byte count — likely a binary or
/// large generated artefact that wouldn't render usefully anyway.
pub const PREVIEW_MAX_BYTES: u64 = 1_000_000;

/// Per-row span table + style table for the preview pane. The
/// `BufferView` consumer takes `Vec<Vec<Span>>` plus a resolver
/// closure mapping `style: u32` → ratatui `Style`; both live here so
/// the renderer can wire them together cheaply.
#[derive(Default)]
pub struct PreviewSpans {
    /// One vec per buffer row, each entry covering a half-open byte
    /// range with an opaque style id.
    pub by_row: Vec<Vec<BufferSpan>>,
    /// Style id → ratatui style. Index with the `style` field of each
    /// `BufferSpan`.
    pub styles: Vec<RatStyle>,
}

impl PreviewSpans {
    /// Build from flat byte-range styled spans (app-side highlighter feeds this).
    ///
    /// `ranges` is a slice of `(byte_range, style)` pairs covering the
    /// raw file bytes in `bytes`. Each range is split across rows at `\n`
    /// boundaries and stored as local-byte-offset `BufferSpan` entries.
    pub fn from_byte_ranges(ranges: &[(std::ops::Range<usize>, RatStyle)], bytes: &[u8]) -> Self {
        let mut row_starts: Vec<usize> = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                row_starts.push(i + 1);
            }
        }
        let row_count = row_starts.len();

        let mut styles: Vec<RatStyle> = Vec::new();
        let mut by_row: Vec<Vec<BufferSpan>> = vec![Vec::new(); row_count];

        for (byte_range, rat) in ranges {
            let style_id = match styles.iter().position(|s| s == rat) {
                Some(i) => i,
                None => {
                    styles.push(*rat);
                    styles.len() - 1
                }
            } as u32;

            let span_start = byte_range.start;
            let span_end = byte_range.end;
            let start_row = row_starts
                .partition_point(|&rs| rs <= span_start)
                .saturating_sub(1);
            let mut row = start_row;
            while row < row_count {
                let row_byte_start = row_starts[row];
                let row_byte_end = row_starts
                    .get(row + 1)
                    .map(|&s| s.saturating_sub(1))
                    .unwrap_or(bytes.len());
                if row_byte_start >= span_end {
                    break;
                }
                let local_start = span_start.saturating_sub(row_byte_start);
                let local_end = span_end.min(row_byte_end) - row_byte_start;
                if local_end > local_start {
                    by_row[row].push(BufferSpan::new(local_start, local_end, style_id));
                }
                row += 1;
            }
        }

        PreviewSpans { by_row, styles }
    }
}

/// Build `PreviewSpans` from a flat list of highlight spans using a
/// `resolve_style` closure instead of a `&dyn Theme` reference.
///
/// `resolve_style` receives the capture name (e.g. `"keyword"`) and
/// returns `Some(Style)` when the theme has a mapping for that name.
/// This keeps `hjkl-picker` free of a `hjkl-tree-sitter` dependency —
/// callers supply the resolver from their own theme binding.
pub fn build_preview_spans<F>(
    flat: &[(std::ops::Range<usize>, &str)],
    bytes: &[u8],
    resolve_style: F,
) -> PreviewSpans
where
    F: Fn(&str) -> Option<RatStyle>,
{
    let mut row_starts: Vec<usize> = vec![0];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            row_starts.push(i + 1);
        }
    }
    let row_count = row_starts.len();

    let mut styles: Vec<RatStyle> = Vec::new();
    let mut by_row: Vec<Vec<BufferSpan>> = vec![Vec::new(); row_count];

    for (byte_range, capture) in flat {
        let Some(rat) = resolve_style(capture) else {
            continue;
        };
        let style_id = match styles.iter().position(|s| *s == rat) {
            Some(i) => i,
            None => {
                styles.push(rat);
                styles.len() - 1
            }
        } as u32;
        let span_start = byte_range.start;
        let span_end = byte_range.end;
        let start_row = row_starts
            .partition_point(|&rs| rs <= span_start)
            .saturating_sub(1);
        let mut row = start_row;
        while row < row_count {
            let row_byte_start = row_starts[row];
            let row_byte_end = row_starts
                .get(row + 1)
                .map(|&s| s.saturating_sub(1))
                .unwrap_or(bytes.len());
            if row_byte_start >= span_end {
                break;
            }
            let local_start = span_start.saturating_sub(row_byte_start);
            let local_end = span_end.min(row_byte_end) - row_byte_start;
            if local_end > local_start {
                by_row[row].push(BufferSpan::new(local_start, local_end, style_id));
            }
            row += 1;
        }
    }
    PreviewSpans { by_row, styles }
}

/// Load a single file for the preview pane. Returns `(content, status)`.
pub fn load_preview(abs: &Path) -> (String, String) {
    let meta = match std::fs::metadata(abs) {
        Ok(m) => m,
        Err(e) => return (String::new(), format!("{e}")),
    };
    if meta.len() > PREVIEW_MAX_BYTES {
        let mb = meta.len() as f64 / 1_048_576.0;
        return (String::new(), format!("{mb:.1}MB — too large"));
    }
    let bytes = match std::fs::read(abs) {
        Ok(b) => b,
        Err(e) => return (String::new(), format!("{e}")),
    };
    let scan_end = bytes.len().min(8192);
    if bytes[..scan_end].contains(&0u8) {
        return (String::new(), "binary".into());
    }
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return (String::new(), "non-utf8".into()),
    };
    let truncated: String = text
        .lines()
        .take(PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    (truncated, String::new())
}
