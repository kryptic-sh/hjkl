//! Renderer-agnostic markdown event stream and theming hooks.
//!
//! Parses CommonMark + common GFM extensions (tables, task lists,
//! strikethrough, footnotes) into a flat [`Event`] stream. No renderer types
//! leak out — backends (ratatui, floem, …) consume `&[Event]` independently.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_markdown::{parse, Event};
//!
//! let events = parse("# Hello\n\nworld");
//! assert!(events.iter().any(|e| matches!(e, Event::Heading { level: 1, .. })));
//! ```

use pulldown_cmark::{Options, Parser, Tag, TagEnd};

// ── Public event type ─────────────────────────────────────────────────────────

/// Column alignment for an [`Event::Table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnAlign {
    /// No explicit alignment.
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// A single logical unit of rendered markdown content.
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// Plain text fragment. May contain inline styling context.
    Text {
        content: String,
        /// True when this text span is inside a `**bold**` run.
        bold: bool,
        /// True when inside an `*italic*` run.
        italic: bool,
        /// True when inside a `~~strikethrough~~` run.
        strikethrough: bool,
        /// True when inside a `` `code` `` span.
        code_span: bool,
    },
    /// A heading line. `text` is the flattened heading content.
    Heading {
        /// ATX heading level: 1 = `#`, 2 = `##`, …, 6 = `######`.
        level: u8,
        text: String,
    },
    /// A fenced or indented code block.
    CodeBlock {
        /// Language hint from the fence (e.g. `"rust"`), empty if none.
        lang: String,
        /// Raw code content — newlines preserved.
        content: String,
    },
    /// A thematic break (`---` / `***`).
    Rule,
    /// Start of an unordered or ordered list item. Subsequent `Text` /
    /// `CodeBlock` events belong to this item until the next `ListItem` or a
    /// non-list event.
    ListItem {
        /// Nesting depth, 0 for a top-level item.
        depth: u8,
        /// Bullet character for unordered (`'-'`, `'*'`, `'+'`); `'\0'` for ordered.
        bullet: char,
        /// 1-based ordinal for ordered lists; 0 for unordered.
        number: u64,
        /// Task-list checkbox state: `Some(true)` = `[x]`, `Some(false)` = `[ ]`,
        /// `None` = not a task item.
        task: Option<bool>,
    },
    /// Blank separator between block elements (paragraph / heading / rule).
    Blank,
    /// A hyperlink.
    Link {
        /// Display text.
        text: String,
        /// Raw destination URL.
        url: String,
    },
    /// An image reference.
    Image {
        /// Alt text.
        alt: String,
        /// Raw source URL.
        url: String,
    },
    /// Start of a `> blockquote`. Content events until the matching
    /// [`Event::BlockQuoteEnd`] belong to the quote (may nest).
    BlockQuoteStart,
    /// End of a blockquote.
    BlockQuoteEnd,
    /// A GFM table. Cells are flattened to plain text.
    Table {
        /// Per-column alignment.
        aligns: Vec<ColumnAlign>,
        /// Header cells.
        header: Vec<String>,
        /// Body rows of cells.
        rows: Vec<Vec<String>>,
    },
}

// ── Theming hooks ─────────────────────────────────────────────────────────────

/// Theming slots consumed by rendering backends.
///
/// All fields are opaque `u32` color tokens (sRGB packed as `0xRRGGBB`).
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MdThemeSlots {
    /// Normal body text foreground.
    pub text: u32,
    /// Heading foreground (level-1).
    pub heading1: u32,
    /// Heading foreground (level-2 … 6).
    pub heading: u32,
    /// Inline code span foreground.
    pub code_span: u32,
    /// Code block foreground.
    pub code_block: u32,
    /// Hyperlink foreground.
    pub link: u32,
    /// List bullet / ordinal foreground.
    pub list_bullet: u32,
    /// Bold text foreground.
    pub bold: u32,
    /// Italic text foreground.
    pub italic: u32,
}

impl MdThemeSlots {
    /// Minimal dark defaults (Catppuccin-ish palette).
    pub fn dark() -> Self {
        Self {
            text: 0xcdd6f4,      // lavender
            heading1: 0xcba6f7,  // mauve
            heading: 0x89b4fa,   // blue
            code_span: 0xa6e3a1, // green
            code_block: 0xa6e3a1,
            link: 0x89dceb,        // sky
            list_bullet: 0xf38ba8, // red
            bold: 0xfab387,        // peach
            italic: 0xf9e2af,      // yellow
        }
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a CommonMark + GFM string into a flat [`Event`] stream.
///
/// Enables tables, task lists, strikethrough, and footnotes. Inline emphasis
/// state (`bold`, `italic`, `strikethrough`, `code_span`) is tracked and
/// annotated on each `Text` event so backends need not maintain a state machine.
pub fn parse(src: &str) -> Vec<Event> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES;
    let parser = Parser::new_ext(src, opts);

    let mut events: Vec<Event> = Vec::new();
    // Inline state.
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    // Block accumulators.
    let mut heading_level: Option<u8> = None;
    let mut heading_buf = String::new();
    let mut code_lang = String::new();
    let mut code_buf: Option<String> = None;
    let mut link_text = String::new();
    let mut link_url = String::new();
    let mut in_link = false;
    let mut image_alt = String::new();
    let mut image_url = String::new();
    let mut in_image = false;
    // List tracking.
    let mut list_ordered_stack: Vec<bool> = Vec::new();
    let mut list_number_stack: Vec<u64> = Vec::new();
    // Table tracking.
    let mut table_aligns: Vec<ColumnAlign> = Vec::new();
    let mut table_header: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut table_row: Vec<String> = Vec::new();
    let mut cell_buf: Option<String> = None;
    let mut in_table_head = false;

    // Route an inline text fragment to whichever accumulator is active, or emit it.
    macro_rules! sink_text {
        ($s:expr, $code_span:expr) => {{
            let s = $s;
            if let Some(buf) = cell_buf.as_mut() {
                buf.push_str(&s);
            } else if let Some(buf) = code_buf.as_mut() {
                buf.push_str(&s);
            } else if heading_level.is_some() {
                heading_buf.push_str(&s);
            } else if in_link {
                link_text.push_str(&s);
            } else if in_image {
                image_alt.push_str(&s);
            } else {
                events.push(Event::Text {
                    content: s.to_string(),
                    bold,
                    italic,
                    strikethrough: strike,
                    code_span: $code_span,
                });
            }
        }};
    }

    for ev in parser {
        match ev {
            // ── Block-level opens/closes ──────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Heading { level, .. }) => {
                heading_level = Some(level as u8);
                heading_buf.clear();
            }
            pulldown_cmark::Event::End(TagEnd::Heading(_)) => {
                if let Some(lvl) = heading_level.take() {
                    events.push(Event::Heading {
                        level: lvl,
                        text: heading_buf.trim_end().to_string(),
                    });
                    heading_buf.clear();
                    events.push(Event::Blank);
                }
            }
            pulldown_cmark::Event::Start(Tag::CodeBlock(kind)) => {
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(s) => s.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                code_buf = Some(String::new());
            }
            pulldown_cmark::Event::End(TagEnd::CodeBlock) => {
                if let Some(buf) = code_buf.take() {
                    events.push(Event::CodeBlock {
                        lang: code_lang.clone(),
                        content: buf.trim_end_matches('\n').to_string(),
                    });
                    events.push(Event::Blank);
                    code_lang.clear();
                }
            }
            pulldown_cmark::Event::Start(Tag::Paragraph) => {}
            pulldown_cmark::Event::End(TagEnd::Paragraph) => {
                events.push(Event::Blank);
            }
            pulldown_cmark::Event::Rule => {
                events.push(Event::Rule);
                events.push(Event::Blank);
            }
            pulldown_cmark::Event::Start(Tag::BlockQuote(_)) => {
                events.push(Event::BlockQuoteStart);
            }
            pulldown_cmark::Event::End(TagEnd::BlockQuote(_)) => {
                events.push(Event::BlockQuoteEnd);
                events.push(Event::Blank);
            }
            // ── Lists ─────────────────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::List(start)) => {
                list_ordered_stack.push(start.is_some());
                list_number_stack.push(start.unwrap_or(1));
            }
            pulldown_cmark::Event::End(TagEnd::List(_)) => {
                list_ordered_stack.pop();
                list_number_stack.pop();
                events.push(Event::Blank);
            }
            pulldown_cmark::Event::Start(Tag::Item) => {
                let ordered = *list_ordered_stack.last().unwrap_or(&false);
                let number = *list_number_stack.last().unwrap_or(&1);
                // Saturate instead of `as`-truncating: 256 levels of nesting
                // must not wrap the depth back to 0.
                let depth =
                    u8::try_from(list_ordered_stack.len().saturating_sub(1)).unwrap_or(u8::MAX);
                events.push(Event::ListItem {
                    depth,
                    bullet: if ordered { '\0' } else { '-' },
                    number,
                    task: None,
                });
                if let Some(n) = list_number_stack.last_mut() {
                    *n += 1;
                }
            }
            pulldown_cmark::Event::End(TagEnd::Item) => {}
            pulldown_cmark::Event::TaskListMarker(checked) => {
                if let Some(Event::ListItem { task, .. }) = events.last_mut() {
                    *task = Some(checked);
                }
            }
            // ── Tables ────────────────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Table(aligns)) => {
                table_aligns = aligns
                    .iter()
                    .map(|a| match a {
                        pulldown_cmark::Alignment::Left => ColumnAlign::Left,
                        pulldown_cmark::Alignment::Center => ColumnAlign::Center,
                        pulldown_cmark::Alignment::Right => ColumnAlign::Right,
                        pulldown_cmark::Alignment::None => ColumnAlign::None,
                    })
                    .collect();
                table_header.clear();
                table_rows.clear();
            }
            pulldown_cmark::Event::End(TagEnd::Table) => {
                events.push(Event::Table {
                    aligns: std::mem::take(&mut table_aligns),
                    header: std::mem::take(&mut table_header),
                    rows: std::mem::take(&mut table_rows),
                });
                events.push(Event::Blank);
            }
            pulldown_cmark::Event::Start(Tag::TableHead) => {
                in_table_head = true;
                table_row.clear();
            }
            pulldown_cmark::Event::End(TagEnd::TableHead) => {
                table_header = std::mem::take(&mut table_row);
                in_table_head = false;
            }
            pulldown_cmark::Event::Start(Tag::TableRow) => {
                table_row.clear();
            }
            pulldown_cmark::Event::End(TagEnd::TableRow) => {
                if !in_table_head {
                    table_rows.push(std::mem::take(&mut table_row));
                }
            }
            pulldown_cmark::Event::Start(Tag::TableCell) => {
                cell_buf = Some(String::new());
            }
            pulldown_cmark::Event::End(TagEnd::TableCell) => {
                table_row.push(cell_buf.take().unwrap_or_default().trim().to_string());
            }
            // ── Inline emphasis ───────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Strong) => bold = true,
            pulldown_cmark::Event::End(TagEnd::Strong) => bold = false,
            pulldown_cmark::Event::Start(Tag::Emphasis) => italic = true,
            pulldown_cmark::Event::End(TagEnd::Emphasis) => italic = false,
            pulldown_cmark::Event::Start(Tag::Strikethrough) => strike = true,
            pulldown_cmark::Event::End(TagEnd::Strikethrough) => strike = false,
            // ── Links & images ────────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Link { dest_url, .. }) => {
                in_link = true;
                link_text.clear();
                link_url = dest_url.to_string();
            }
            pulldown_cmark::Event::End(TagEnd::Link) => {
                in_link = false;
                if cell_buf.is_some() || code_buf.is_some() || heading_level.is_some() {
                    // The link text was already flattened into the active
                    // block accumulator (table cell / heading); emitting a
                    // standalone Link event here would render the URL as
                    // stray content outside that block.
                } else if in_image {
                    // Link nested in image alt text — fold the text back
                    // into the alt accumulator.
                    image_alt.push_str(&link_text);
                } else {
                    events.push(Event::Link {
                        text: link_text.clone(),
                        url: link_url.clone(),
                    });
                }
                link_text.clear();
                link_url.clear();
            }
            pulldown_cmark::Event::Start(Tag::Image { dest_url, .. }) => {
                in_image = true;
                image_alt.clear();
                image_url = dest_url.to_string();
            }
            pulldown_cmark::Event::End(TagEnd::Image) => {
                in_image = false;
                if cell_buf.is_some() || code_buf.is_some() || heading_level.is_some() {
                    // Alt text already flattened into the active block
                    // accumulator — no standalone Image event.
                } else if in_link {
                    // Image nested in link text — fold the alt back into
                    // the link text accumulator.
                    link_text.push_str(&image_alt);
                } else {
                    events.push(Event::Image {
                        alt: image_alt.clone(),
                        url: image_url.clone(),
                    });
                }
                image_alt.clear();
                image_url.clear();
            }
            // ── Inline code & text ────────────────────────────────────────
            pulldown_cmark::Event::Code(s) => sink_text!(s, true),
            pulldown_cmark::Event::Text(s) => sink_text!(s, false),
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {
                // Mirror sink_text!'s accumulator priority: a break inside a
                // flattened block becomes a single space so words don't glue
                // together. Code blocks carry their own newlines in Text
                // events, so no separator is needed there.
                if let Some(buf) = cell_buf.as_mut() {
                    buf.push(' ');
                } else if code_buf.is_some() {
                } else if heading_level.is_some() {
                    heading_buf.push(' ');
                } else if in_link {
                    link_text.push(' ');
                } else if in_image {
                    image_alt.push(' ');
                } else {
                    events.push(Event::Text {
                        content: "\n".to_string(),
                        bold,
                        italic,
                        strikethrough: strike,
                        code_span: false,
                    });
                }
            }
            _ => {}
        }
    }

    events
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_parsed() {
        let evs = parse("# Hello world");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Heading { level: 1, text } if text == "Hello world"))
        );
    }

    #[test]
    fn code_block_with_lang() {
        let evs = parse("```rust\nfn main() {}\n```");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::CodeBlock { lang, content }
            if lang == "rust" && content.contains("fn main")))
        );
    }

    #[test]
    fn code_span_flag() {
        let evs = parse("`foo`");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Text { code_span: true, content, .. }
            if content == "foo"))
        );
    }

    #[test]
    fn bold_and_strikethrough_flags() {
        assert!(
            parse("**bold**")
                .iter()
                .any(|e| matches!(e, Event::Text { bold: true, .. }))
        );
        assert!(parse("~~gone~~").iter().any(
            |e| matches!(e, Event::Text { strikethrough: true, content, .. } if content == "gone")
        ));
    }

    #[test]
    fn nested_list_depth() {
        let evs = parse("- a\n  - b\n");
        let depths: Vec<u8> = evs
            .iter()
            .filter_map(|e| match e {
                Event::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1], "got {evs:?}");
    }

    #[test]
    fn task_list_markers() {
        let evs = parse("- [x] done\n- [ ] todo\n");
        let tasks: Vec<Option<bool>> = evs
            .iter()
            .filter_map(|e| match e {
                Event::ListItem { task, .. } => Some(*task),
                _ => None,
            })
            .collect();
        assert_eq!(tasks, vec![Some(true), Some(false)], "got {evs:?}");
    }

    #[test]
    fn table_parsed() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let evs = parse(md);
        let table = evs.iter().find_map(|e| match e {
            Event::Table { header, rows, .. } => Some((header, rows)),
            _ => None,
        });
        let (header, rows) = table.expect("a table event");
        assert_eq!(header, &vec!["a".to_string(), "b".to_string()]);
        assert_eq!(rows, &vec![vec!["1".to_string(), "2".to_string()]]);
    }

    #[test]
    fn blockquote_brackets_content() {
        let evs = parse("> quoted\n");
        let start = evs.iter().position(|e| matches!(e, Event::BlockQuoteStart));
        let end = evs.iter().position(|e| matches!(e, Event::BlockQuoteEnd));
        assert!(
            start.is_some() && end.is_some() && start < end,
            "got {evs:?}"
        );
    }

    #[test]
    fn image_event() {
        let evs = parse("![alt text](http://x/y.png)");
        assert!(evs.iter().any(|e| matches!(e, Event::Image { alt, url }
            if alt == "alt text" && url == "http://x/y.png")));
    }

    #[test]
    fn link_event() {
        let evs = parse("[click](https://hjkl.kryptic.sh)");
        assert!(evs.iter().any(|e| matches!(e, Event::Link { url, .. }
            if url == "https://hjkl.kryptic.sh")));
    }

    #[test]
    fn rule_event() {
        assert!(parse("---").iter().any(|e| matches!(e, Event::Rule)));
    }

    #[test]
    fn dark_theme_slots_nonzero() {
        let t = MdThemeSlots::dark();
        assert_ne!(t.text, 0);
        assert_ne!(t.heading1, 0);
    }

    #[test]
    fn empty_input_no_panic() {
        let evs = parse("");
        assert!(evs.is_empty() || evs.iter().all(|e| matches!(e, Event::Blank)));
    }

    #[test]
    fn link_in_heading_no_stray_event() {
        // The link text is flattened into the heading; no standalone Link
        // event may leak out (it used to render as a bare URL line).
        let evs = parse("# [Click](https://x.example)");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Heading { level: 1, text } if text == "Click")),
            "got {evs:?}"
        );
        assert!(
            !evs.iter().any(|e| matches!(e, Event::Link { .. })),
            "stray Link event: {evs:?}"
        );
    }

    #[test]
    fn link_in_table_cell_no_stray_event() {
        let md = "| a |\n|---|\n| [x](http://y) |\n";
        let evs = parse(md);
        let rows = evs
            .iter()
            .find_map(|e| match e {
                Event::Table { rows, .. } => Some(rows.clone()),
                _ => None,
            })
            .expect("a table event");
        assert_eq!(rows, vec![vec!["x".to_string()]], "got {evs:?}");
        assert!(
            !evs.iter().any(|e| matches!(e, Event::Link { .. })),
            "stray Link event: {evs:?}"
        );
    }

    #[test]
    fn image_in_table_cell_no_stray_event() {
        let md = "| a |\n|---|\n| ![alt](http://y.png) |\n";
        let evs = parse(md);
        assert!(
            !evs.iter().any(|e| matches!(e, Event::Image { .. })),
            "stray Image event: {evs:?}"
        );
    }

    #[test]
    fn setext_heading_softbreak_keeps_word_separator() {
        // Setext headings can span source lines; the soft break must not
        // glue the words together.
        let evs = parse("foo\nbar\n===\n");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Heading { level: 1, text } if text == "foo bar")),
            "got {evs:?}"
        );
    }

    #[test]
    fn link_text_softbreak_keeps_word_separator() {
        let evs = parse("[foo\nbar](http://x)");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Link { text, .. } if text == "foo bar")),
            "got {evs:?}"
        );
    }

    #[test]
    fn deep_list_depth_never_wraps() {
        // 300 nesting levels: depth is a u8, so it must saturate at 255,
        // never wrap back toward 0.
        let mut md = String::new();
        for i in 0..300usize {
            md.push_str(&"  ".repeat(i));
            md.push_str("- x\n");
        }
        let depths: Vec<u8> = parse(&md)
            .iter()
            .filter_map(|e| match e {
                Event::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert!(!depths.is_empty());
        assert!(
            depths.windows(2).all(|w| w[1] >= w[0]),
            "depth wrapped: {depths:?}"
        );
    }
}
