//! Ratatui adapter for `hjkl-markdown`.
//!
//! Converts a [`hjkl_markdown::Event`] stream into a `Vec<ratatui::text::Line>`
//! suitable for rendering in a [`ratatui::widgets::Paragraph`] or similar widget.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_markdown::parse;
//! use hjkl_markdown_tui::{MdTheme, to_lines};
//!
//! let events = parse("# Title\n\nhello `world`");
//! let theme = MdTheme::default();
//! let lines = to_lines(&events, &theme, 60);
//! assert!(!lines.is_empty());
//! ```

use hjkl_markdown::Event;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// ── MdTheme ───────────────────────────────────────────────────────────────────

/// Color slots for markdown rendering in ratatui.
///
/// All fields are raw `ratatui::style::Color` values.  Build from your app's
/// theme palette or use [`MdTheme::default`] for a sensible dark fallback.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MdTheme {
    /// Normal body text.
    pub text: Color,
    /// Level-1 heading foreground.
    pub heading1: Color,
    /// Level 2–6 heading foreground.
    pub heading: Color,
    /// Inline code span foreground.
    pub code_span: Color,
    /// Code block foreground.
    pub code_block: Color,
    /// Hyperlink foreground.
    pub link: Color,
    /// List bullet / ordinal foreground.
    pub list_bullet: Color,
    /// Bold text foreground.
    pub bold: Color,
    /// Italic text foreground.
    pub italic: Color,
    /// Horizontal rule foreground.
    pub rule: Color,
}

impl MdTheme {
    /// Construct a theme from explicit color values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        text: Color,
        heading1: Color,
        heading: Color,
        code_span: Color,
        code_block: Color,
        link: Color,
        list_bullet: Color,
        bold: Color,
        italic: Color,
        rule: Color,
    ) -> Self {
        Self {
            text,
            heading1,
            heading,
            code_span,
            code_block,
            link,
            list_bullet,
            bold,
            italic,
            rule,
        }
    }
}

impl Default for MdTheme {
    /// Dark fallback (Catppuccin-ish).
    fn default() -> Self {
        Self {
            text: Color::Rgb(0xcd, 0xd6, 0xf4),
            heading1: Color::Rgb(0xcb, 0xa6, 0xf7),
            heading: Color::Rgb(0x89, 0xb4, 0xfa),
            code_span: Color::Rgb(0xa6, 0xe3, 0xa1),
            code_block: Color::Rgb(0xa6, 0xe3, 0xa1),
            link: Color::Rgb(0x89, 0xdc, 0xeb),
            list_bullet: Color::Rgb(0xf3, 0x8b, 0xa8),
            bold: Color::Rgb(0xfa, 0xb3, 0x87),
            italic: Color::Rgb(0xf9, 0xe2, 0xaf),
            rule: Color::Rgb(0x58, 0x5b, 0x70),
        }
    }
}

// ── to_lines ──────────────────────────────────────────────────────────────────

/// Convert a [`hjkl_markdown::Event`] slice into `ratatui::text::Line` rows.
///
/// `width` is the available column count — long lines are wrapped at word
/// boundaries. Blank events become empty separator lines.
pub fn to_lines(events: &[Event], theme: &MdTheme, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Current line accumulator.
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    let flush = |spans: &mut Vec<Span<'static>>, lines: &mut Vec<Line<'static>>| {
        if !spans.is_empty() {
            lines.push(Line::from(std::mem::take(spans)));
        }
    };

    for ev in events {
        match ev {
            Event::Heading { level, text } => {
                flush(&mut current_spans, &mut lines);
                let fg = if *level == 1 {
                    theme.heading1
                } else {
                    theme.heading
                };
                let prefix = "#".repeat(*level as usize);
                let label = format!("{prefix} {text}");
                let style = Style::default().fg(fg).add_modifier(Modifier::BOLD);
                for wrapped in wrap_str(&label, width as usize) {
                    lines.push(Line::from(vec![Span::styled(wrapped, style)]));
                }
            }
            Event::CodeBlock { lang, content } => {
                flush(&mut current_spans, &mut lines);
                if !lang.is_empty() {
                    let lang_line = format!("[{lang}]");
                    lines.push(Line::from(vec![Span::styled(
                        lang_line,
                        Style::default()
                            .fg(theme.code_block)
                            .add_modifier(Modifier::DIM),
                    )]));
                }
                let style = Style::default().fg(theme.code_block);
                for src_line in content.lines() {
                    for wrapped in wrap_str(src_line, width as usize) {
                        lines.push(Line::from(vec![Span::styled(wrapped, style)]));
                    }
                }
            }
            Event::Rule => {
                flush(&mut current_spans, &mut lines);
                let rule_str = "─".repeat(width.saturating_sub(0) as usize);
                lines.push(Line::from(vec![Span::styled(
                    rule_str,
                    Style::default().fg(theme.rule),
                )]));
            }
            Event::ListItem { bullet, number } => {
                flush(&mut current_spans, &mut lines);
                let prefix = if *bullet == '\0' {
                    format!("{number}. ")
                } else {
                    format!("{bullet} ")
                };
                current_spans.push(Span::styled(prefix, Style::default().fg(theme.list_bullet)));
            }
            Event::Blank => {
                flush(&mut current_spans, &mut lines);
                lines.push(Line::default());
            }
            Event::Link { text, url } => {
                let label = if text.is_empty() {
                    url.clone()
                } else {
                    format!("{text} <{url}>")
                };
                let style = Style::default()
                    .fg(theme.link)
                    .add_modifier(Modifier::UNDERLINED);
                for wrapped in wrap_str(&label, width as usize) {
                    current_spans.push(Span::styled(wrapped, style));
                }
            }
            Event::Text {
                content,
                bold,
                italic,
                code_span,
            } => {
                let fg = if *code_span {
                    theme.code_span
                } else if *bold {
                    theme.bold
                } else if *italic {
                    theme.italic
                } else {
                    theme.text
                };
                let mut style = Style::default().fg(fg);
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if *italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if *code_span {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Newlines in text → flush + new line.
                for (i, part) in content.split('\n').enumerate() {
                    if i > 0 {
                        flush(&mut current_spans, &mut lines);
                    }
                    if !part.is_empty() {
                        for wrapped in wrap_str(part, width as usize) {
                            current_spans.push(Span::styled(wrapped, style));
                        }
                    }
                }
            }
            // Forward compat: ignore unknown variants from #[non_exhaustive] Event.
            _ => {}
        }
    }

    flush(&mut current_spans, &mut lines);

    // Strip trailing blank lines.
    while lines
        .last()
        .map(|l: &Line<'_>| l.spans.is_empty())
        .unwrap_or(false)
    {
        lines.pop();
    }

    lines
}

/// Naive word-wrap: split `s` into chunks that fit in `width` columns.
/// Falls back to hard-breaking if a single word exceeds `width`.
fn wrap_str(s: &str, width: usize) -> Vec<String> {
    if width == 0 || s.len() <= width {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for word in s.split_whitespace() {
        let needed = if current.is_empty() {
            word.len()
        } else {
            current.len() + 1 + word.len()
        };
        if needed > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        if word.len() > width {
            // Hard-break long token.
            for chunk in word.as_bytes().chunks(width) {
                let s = String::from_utf8_lossy(chunk).to_string();
                if current.len() + s.len() > width && !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                current.push_str(&s);
            }
        } else {
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_markdown::parse;

    #[test]
    fn smoke_empty() {
        let lines = to_lines(&[], &MdTheme::default(), 80);
        assert!(lines.is_empty());
    }

    #[test]
    fn heading_produces_lines() {
        let evs = parse("# Hello");
        let lines = to_lines(&evs, &MdTheme::default(), 80);
        assert!(!lines.is_empty());
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Hello"), "heading text not found: {text:?}");
    }

    #[test]
    fn code_block_lines() {
        let evs = parse("```rust\nfn main() {}\n```");
        let lines = to_lines(&evs, &MdTheme::default(), 80);
        assert!(
            lines
                .iter()
                .any(|l| l.spans.iter().any(|s| s.content.contains("fn main")))
        );
    }

    #[test]
    fn wrap_long_line() {
        let chunks = wrap_str("hello world foo bar baz", 10);
        for c in &chunks {
            assert!(c.len() <= 10, "chunk too wide: {c:?}");
        }
    }

    #[test]
    fn default_theme_has_colors() {
        let t = MdTheme::default();
        assert!(matches!(t.text, Color::Rgb(_, _, _)));
        assert!(matches!(t.heading1, Color::Rgb(_, _, _)));
    }
}
