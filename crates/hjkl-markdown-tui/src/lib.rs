//! Ratatui adapter for `hjkl-markdown`.
//!
//! Converts a [`hjkl_markdown::Event`] stream into a `Vec<ratatui::text::Line>`
//! suitable for rendering in a [`ratatui::widgets::Paragraph`] or similar widget.
//! Supports headings, emphasis (bold/italic/strikethrough), inline + fenced
//! code, links, images, nested + task lists, blockquotes, rules, and tables.
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

use hjkl_markdown::{ColumnAlign, Event};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    /// Horizontal rule / blockquote bar / table border foreground.
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
/// `width` is the available column count. Block elements (headings, code, rules,
/// tables) are laid out to fit; inline prose is left for the host
/// `Paragraph`/widget to wrap.
pub fn to_lines(events: &[Event], theme: &MdTheme, width: u16) -> Vec<Line<'static>> {
    let mut r = Renderer {
        lines: Vec::new(),
        cur: Vec::new(),
        theme,
        width,
        quote: 0,
    };
    for ev in events {
        r.event(ev);
    }
    r.flush();
    // Strip trailing blank lines.
    while r
        .lines
        .last()
        .map(|l| l.spans.iter().all(|s| s.content.trim().is_empty()))
        .unwrap_or(false)
    {
        r.lines.pop();
    }
    r.lines
}

/// Stateful markdown → ratatui line renderer (tracks blockquote nesting).
struct Renderer<'a> {
    lines: Vec<Line<'static>>,
    cur: Vec<Span<'static>>,
    theme: &'a MdTheme,
    width: u16,
    quote: usize,
}

impl Renderer<'_> {
    /// Columns available for content after the blockquote bar.
    fn inner_width(&self) -> usize {
        (self.width as usize).saturating_sub(self.quote * 2).max(1)
    }

    fn quote_bar(&self) -> Option<Span<'static>> {
        (self.quote > 0).then(|| {
            Span::styled(
                "│ ".repeat(self.quote),
                Style::default().fg(self.theme.rule),
            )
        })
    }

    /// Flush any pending inline spans as a line.
    fn flush(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        if let Some(bar) = self.quote_bar() {
            spans.push(bar);
        }
        spans.append(&mut self.cur);
        self.lines.push(Line::from(spans));
    }

    /// Flush pending inline, then push `content` as its own (quote-prefixed) line.
    fn push_line(&mut self, content: Vec<Span<'static>>) {
        self.flush();
        let mut spans = Vec::new();
        if let Some(bar) = self.quote_bar() {
            spans.push(bar);
        }
        spans.extend(content);
        self.lines.push(Line::from(spans));
    }

    fn blank(&mut self) {
        self.flush();
        match self.quote_bar() {
            Some(bar) => self.lines.push(Line::from(bar)),
            None => self.lines.push(Line::default()),
        }
    }

    fn event(&mut self, ev: &Event) {
        match ev {
            Event::Heading { level, text } => {
                let fg = if *level == 1 {
                    self.theme.heading1
                } else {
                    self.theme.heading
                };
                let style = Style::default().fg(fg).add_modifier(Modifier::BOLD);
                let label = format!("{} {text}", "#".repeat(*level as usize));
                for w in wrap_str(&label, self.inner_width()) {
                    self.push_line(vec![Span::styled(w, style)]);
                }
            }
            Event::CodeBlock { lang, content } => {
                self.flush();
                if !lang.is_empty() {
                    self.push_line(vec![Span::styled(
                        format!("[{lang}]"),
                        Style::default()
                            .fg(self.theme.code_block)
                            .add_modifier(Modifier::DIM),
                    )]);
                }
                let style = Style::default().fg(self.theme.code_block);
                for src in content.lines() {
                    // Code is whitespace-significant: hard-wrap by display
                    // width so leading indentation and internal spaces survive
                    // (word-wrap would collapse them).
                    for w in hard_wrap(src, self.inner_width()) {
                        self.push_line(vec![Span::styled(w, style)]);
                    }
                }
            }
            Event::Rule => {
                let bar = "─".repeat(self.inner_width());
                self.push_line(vec![Span::styled(
                    bar,
                    Style::default().fg(self.theme.rule),
                )]);
            }
            Event::ListItem {
                depth,
                bullet,
                number,
                task,
            } => {
                self.flush();
                let indent = "  ".repeat(*depth as usize);
                let (marker, mstyle) = match task {
                    Some(true) => (
                        "[x] ".to_string(),
                        Style::default().fg(self.theme.list_bullet),
                    ),
                    Some(false) => (
                        "[ ] ".to_string(),
                        Style::default().fg(self.theme.list_bullet),
                    ),
                    None if *bullet == '\0' => (
                        format!("{number}. "),
                        Style::default().fg(self.theme.list_bullet),
                    ),
                    None => (
                        "• ".to_string(),
                        Style::default().fg(self.theme.list_bullet),
                    ),
                };
                if !indent.is_empty() {
                    self.cur.push(Span::raw(indent));
                }
                self.cur.push(Span::styled(marker, mstyle));
            }
            Event::Blank => self.blank(),
            Event::Link { text, url } => {
                let label = if text.is_empty() {
                    url.clone()
                } else {
                    format!("{text} <{url}>")
                };
                self.cur.push(Span::styled(
                    label,
                    Style::default()
                        .fg(self.theme.link)
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            Event::Image { alt, url } => {
                // No emoji — glyph width/support varies across terminals.
                let label = if alt.is_empty() {
                    format!("image: <{url}>")
                } else {
                    format!("image: {alt} <{url}>")
                };
                self.cur
                    .push(Span::styled(label, Style::default().fg(self.theme.link)));
            }
            Event::BlockQuoteStart => {
                self.flush();
                self.quote += 1;
            }
            Event::BlockQuoteEnd => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            Event::Table {
                aligns,
                header,
                rows,
            } => self.render_table(aligns, header, rows),
            Event::Text {
                content,
                bold,
                italic,
                strikethrough,
                code_span,
            } => {
                let fg = if *code_span {
                    self.theme.code_span
                } else if *bold {
                    self.theme.bold
                } else if *italic {
                    self.theme.italic
                } else {
                    self.theme.text
                };
                let mut style = Style::default().fg(fg);
                if *bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if *italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if *strikethrough {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
                if *code_span {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Embedded newlines (soft/hard breaks) split into separate lines.
                for (i, part) in content.split('\n').enumerate() {
                    if i > 0 {
                        self.flush();
                    }
                    if !part.is_empty() {
                        self.cur.push(Span::styled(part.to_string(), style));
                    }
                }
            }
            _ => {}
        }
    }

    fn render_table(&mut self, aligns: &[ColumnAlign], header: &[String], rows: &[Vec<String>]) {
        self.flush();
        let mut ncols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        if ncols == 0 {
            return;
        }

        // How many columns can fit at a minimum content width? Each column
        // costs MIN_COL content + 2 padding + 1 border; plus one leading
        // border. Columns beyond that are elided behind a trailing "…" so the
        // table never renders wider than the viewport (which the host clips).
        const MIN_COL: usize = 3;
        let per_col = MIN_COL + 3;
        let usable = self.inner_width().saturating_sub(1);
        let max_cols = (usable / per_col).max(1);

        // Owned working copies so we can truncate + append an ellipsis column.
        let mut header: Vec<String> = header.to_vec();
        let mut rows: Vec<Vec<String>> = rows.to_vec();
        let mut aligns: Vec<ColumnAlign> = aligns.to_vec();
        if ncols > max_cols {
            let keep = max_cols.saturating_sub(1).max(1); // reserve last col for "…"
            header.truncate(keep);
            header.push("\u{2026}".to_string());
            for r in rows.iter_mut() {
                r.truncate(keep);
                while r.len() < keep {
                    r.push(String::new());
                }
                r.push("\u{2026}".to_string());
            }
            aligns.truncate(keep);
            aligns.push(ColumnAlign::default());
            ncols = keep + 1;
        }
        let aligns = aligns.as_slice();

        // Natural column widths from header + body cells.
        let mut col_w = vec![0usize; ncols];
        let measure = |cells: &[String], col_w: &mut [usize]| {
            for (i, c) in cells.iter().enumerate().take(ncols) {
                col_w[i] = col_w[i].max(UnicodeWidthStr::width(c.as_str()));
            }
        };
        measure(&header, &mut col_w);
        for r in &rows {
            measure(r, &mut col_w);
        }

        // Fit to the available width: each column has 2 padding spaces plus a
        // border, and one trailing border.
        let overhead = 3 * ncols + 1;
        let budget = self.inner_width().saturating_sub(overhead).max(ncols);
        let total: usize = col_w.iter().sum();
        if total > budget && total > 0 {
            for w in col_w.iter_mut() {
                *w = (((*w as f64) / total as f64) * budget as f64).floor() as usize;
                *w = (*w).max(3);
            }
        }

        let border = Style::default().fg(self.theme.rule);
        let head_style = Style::default()
            .fg(self.theme.heading)
            .add_modifier(Modifier::BOLD);
        let body_style = Style::default().fg(self.theme.text);

        self.push_line(vec![Span::styled(sep_line(&col_w, '┌', '┬', '┐'), border)]);
        self.push_line(self.row_spans(&header, &col_w, aligns, head_style, border));
        self.push_line(vec![Span::styled(sep_line(&col_w, '├', '┼', '┤'), border)]);
        for r in &rows {
            self.push_line(self.row_spans(r, &col_w, aligns, body_style, border));
        }
        self.push_line(vec![Span::styled(sep_line(&col_w, '└', '┴', '┘'), border)]);
    }

    fn row_spans(
        &self,
        cells: &[String],
        col_w: &[usize],
        aligns: &[ColumnAlign],
        cell_style: Style,
        border: Style,
    ) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled("│", border)];
        for (i, w) in col_w.iter().enumerate() {
            let cell = cells.get(i).map(String::as_str).unwrap_or("");
            let align = aligns.get(i).copied().unwrap_or_default();
            spans.push(Span::styled(
                format!(" {} ", pad(cell, *w, align)),
                cell_style,
            ));
            spans.push(Span::styled("│", border));
        }
        spans
    }
}

/// Build a horizontal table separator like `├───┼───┤`.
fn sep_line(col_w: &[usize], left: char, mid: char, right: char) -> String {
    let mut s = String::new();
    s.push(left);
    for (i, w) in col_w.iter().enumerate() {
        s.push_str(&"─".repeat(w + 2));
        s.push(if i + 1 < col_w.len() { mid } else { right });
    }
    s
}

/// Truncate (with `…`) and pad `s` to `w` display columns per `align`.
fn pad(s: &str, w: usize, align: ColumnAlign) -> String {
    let n = UnicodeWidthStr::width(s);
    let s = if n > w && w > 0 {
        let head = trunc_to_width(s, w.saturating_sub(1));
        format!("{head}…")
    } else {
        s.to_string()
    };
    let len = UnicodeWidthStr::width(s.as_str());
    let padn = w.saturating_sub(len);
    match align {
        ColumnAlign::Right => format!("{}{s}", " ".repeat(padn)),
        ColumnAlign::Center => {
            let l = padn / 2;
            format!("{}{s}{}", " ".repeat(l), " ".repeat(padn - l))
        }
        _ => format!("{s}{}", " ".repeat(padn)),
    }
}

/// Word-wrap `s` into chunks that fit in `width` display columns, hard-breaking
/// any single word that is itself wider than `width`.
fn wrap_str(s: &str, width: usize) -> Vec<String> {
    if width == 0 || UnicodeWidthStr::width(s) <= width {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for word in s.split_whitespace() {
        let ww = UnicodeWidthStr::width(word);
        let needed = if cur.is_empty() { ww } else { cur_w + 1 + ww };
        if needed > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if !cur.is_empty() {
            cur.push(' ');
            cur_w += 1;
        }
        if ww > width {
            for ch in word.chars() {
                let ch_w = ch.width().unwrap_or(1).max(1);
                if cur_w + ch_w > width && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(ch);
                cur_w += ch_w;
            }
        } else {
            cur.push_str(word);
            cur_w += ww;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Hard-wrap `s` into chunks of at most `width` display columns, preserving
/// every character (including leading indentation and internal whitespace).
/// Unlike [`wrap_str`], this never collapses spaces — used for code blocks
/// where whitespace is significant.
fn hard_wrap(s: &str, width: usize) -> Vec<String> {
    if width == 0 || UnicodeWidthStr::width(s) <= width {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        let ch_w = ch.width().unwrap_or(1).max(1);
        if cur_w + ch_w > width && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(ch);
        cur_w += ch_w;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Take characters from the front of `s` whose combined display width ≤ `max_w`,
/// returning them as a new String.
fn trunc_to_width(s: &str, max_w: usize) -> String {
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(1).max(1);
        if w + cw > max_w {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_markdown::parse;

    fn flat(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn smoke_empty() {
        let lines = to_lines(&[], &MdTheme::default(), 80);
        assert!(lines.is_empty());
    }

    #[test]
    fn heading_produces_lines() {
        let lines = to_lines(&parse("# Hello"), &MdTheme::default(), 80);
        assert!(flat(&lines).contains("Hello"));
    }

    #[test]
    fn code_block_lines() {
        let lines = to_lines(
            &parse("```rust\nfn main() {}\n```"),
            &MdTheme::default(),
            80,
        );
        assert!(flat(&lines).contains("fn main"));
    }

    #[test]
    fn code_block_preserves_indentation() {
        // Leading indentation and internal double-spaces must survive.
        let src = "```rust\n    let x = 1;\nif a  b {}\n```";
        let out = flat(&to_lines(&parse(src), &MdTheme::default(), 80));
        assert!(
            out.lines().any(|l| l.starts_with("    let x = 1;")),
            "indentation lost: {out:?}"
        );
        assert!(
            out.contains("if a  b {}"),
            "internal spaces collapsed: {out:?}"
        );
    }

    #[test]
    fn hard_wrap_preserves_all_chars() {
        // 20 chars incl. leading spaces; wrap at 8 must keep every char and
        // never exceed the width.
        let chunks = hard_wrap("   abcde  fghij lmno", 8);
        let rejoined: String = chunks.concat();
        assert_eq!(rejoined, "   abcde  fghij lmno");
        for c in &chunks {
            assert!(
                UnicodeWidthStr::width(c.as_str()) <= 8,
                "chunk too wide: {c:?}"
            );
        }
    }

    #[test]
    fn table_elides_columns_when_too_many_to_fit() {
        // 10 columns into a narrow 24-col viewport: must elide with a "…"
        // marker and stay within the viewport width.
        let header = "| a | b | c | d | e | f | g | h | i | j |";
        let sep = "|---|---|---|---|---|---|---|---|---|---|";
        let row = "| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 0 |";
        let md = format!("{header}\n{sep}\n{row}\n");
        let lines = to_lines(&parse(&md), &MdTheme::default(), 24);
        let out = flat(&lines);
        assert!(out.contains('\u{2026}'), "no elision marker: {out}");
        for l in &lines {
            let w: usize = l
                .spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            assert!(w <= 24, "table row overflows viewport ({w} > 24): {out}");
        }
    }

    #[test]
    fn table_renders_box() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let out = flat(&to_lines(&parse(md), &MdTheme::default(), 80));
        assert!(
            out.contains('┌') && out.contains('│'),
            "no box drawing: {out}"
        );
        assert!(out.contains('a') && out.contains('1'));
    }

    #[test]
    fn task_list_checkboxes() {
        let out = flat(&to_lines(
            &parse("- [x] done\n- [ ] todo\n"),
            &MdTheme::default(),
            80,
        ));
        assert!(out.contains("[x]") && out.contains("[ ]"), "{out}");
    }

    #[test]
    fn nested_list_indents() {
        let out = flat(&to_lines(&parse("- a\n  - b\n"), &MdTheme::default(), 80));
        assert!(
            out.lines().any(|l| l.starts_with("  ")),
            "no indent: {out:?}"
        );
    }

    #[test]
    fn blockquote_has_bar() {
        let out = flat(&to_lines(&parse("> hi\n"), &MdTheme::default(), 80));
        assert!(out.contains('│'), "no quote bar: {out}");
    }

    #[test]
    fn wrap_long_line() {
        use unicode_width::UnicodeWidthStr;
        for c in wrap_str("hello world foo bar baz", 10) {
            assert!(
                UnicodeWidthStr::width(c.as_str()) <= 10,
                "chunk too wide: {c:?}"
            );
        }
    }

    #[test]
    fn wrap_wide_chars_do_not_overflow() {
        use unicode_width::UnicodeWidthStr;
        // Double-width chars landing at the boundary must not produce a
        // chunk wider than the requested width.
        for width in 1..=6usize {
            for c in wrap_str("ああああああ", width) {
                assert!(
                    UnicodeWidthStr::width(c.as_str()) <= width.max(2),
                    "chunk {c:?} too wide for width {width}"
                );
            }
            if width >= 2 {
                for c in wrap_str("ああああああ", width) {
                    assert!(
                        UnicodeWidthStr::width(c.as_str()) <= width,
                        "chunk {c:?} too wide for width {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn default_theme_has_colors() {
        let t = MdTheme::default();
        assert!(matches!(t.text, Color::Rgb(_, _, _)));
    }

    #[test]
    fn table_emojis_align_columns() {
        // Issue #270: emojis are multi-width (display width 2) but chars().count()
        // returns 1, causing column width under-measurement and border misalignment.
        use unicode_width::UnicodeWidthStr;

        let md = "| status | desc |\n|---|---|\n| ✅ | pass |\n| ❌ | fail |\n";
        let out = flat(&to_lines(&parse(md), &MdTheme::default(), 80));

        // Every row must have the same *display* width (not scalar count).
        let widths: Vec<usize> = out.lines().map(UnicodeWidthStr::width).collect();
        let first = widths.first().copied().unwrap_or(0);
        for (i, w) in widths.iter().enumerate() {
            assert_eq!(
                *w, first,
                "table row {i} display-width {w} differs from expected {first}; full output:\n{out}"
            );
        }
    }
}
