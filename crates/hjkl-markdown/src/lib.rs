//! Renderer-agnostic markdown event stream and theming hooks.
//!
//! Parses CommonMark markdown into a flat [`Event`] stream. No renderer types
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
        /// Bullet character for unordered (`'-'`, `'*'`, `'+'`); `'\0'` for ordered.
        bullet: char,
        /// 1-based ordinal for ordered lists; 0 for unordered.
        number: u64,
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

/// Parse a CommonMark string into a flat [`Event`] stream.
///
/// The output is suitable for rendering by any backend (ratatui, floem, …).
/// Inline emphasis state (`bold`, `italic`, `code_span`) is tracked and
/// annotated on each `Text` event so backends need not maintain a state machine.
pub fn parse(src: &str) -> Vec<Event> {
    let opts = Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(src, opts);

    let mut events: Vec<Event> = Vec::new();
    // Inline state machine.
    let mut bold = false;
    let mut italic = false;
    // Block accumulators.
    let mut heading_level: Option<u8> = None;
    let mut heading_buf = String::new();
    let mut code_lang = String::new();
    let mut code_buf: Option<String> = None;
    let mut link_text = String::new();
    let mut link_url = String::new();
    let mut in_link = false;
    // List tracking.
    let mut list_ordered_stack: Vec<bool> = Vec::new();
    let mut list_number_stack: Vec<u64> = Vec::new();

    for ev in parser {
        match ev {
            // ── Block-level opens ─────────────────────────────────────────
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
                events.push(Event::ListItem {
                    bullet: if ordered { '\0' } else { '-' },
                    number,
                });
                // Advance the counter.
                if let Some(n) = list_number_stack.last_mut() {
                    *n += 1;
                }
            }
            pulldown_cmark::Event::End(TagEnd::Item) => {}
            // ── Inline emphasis ───────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Strong) => {
                bold = true;
            }
            pulldown_cmark::Event::End(TagEnd::Strong) => {
                bold = false;
            }
            pulldown_cmark::Event::Start(Tag::Emphasis) => {
                italic = true;
            }
            pulldown_cmark::Event::End(TagEnd::Emphasis) => {
                italic = false;
            }
            pulldown_cmark::Event::Code(s) => {
                if let Some(ref mut buf) = code_buf {
                    buf.push_str(&s);
                } else if heading_level.is_some() {
                    heading_buf.push_str(&s);
                } else {
                    events.push(Event::Text {
                        content: s.to_string(),
                        bold,
                        italic,
                        code_span: true,
                    });
                }
            }
            // ── Links ─────────────────────────────────────────────────────
            pulldown_cmark::Event::Start(Tag::Link { dest_url, .. }) => {
                in_link = true;
                link_text.clear();
                link_url = dest_url.to_string();
            }
            pulldown_cmark::Event::End(TagEnd::Link) => {
                in_link = false;
                events.push(Event::Link {
                    text: link_text.clone(),
                    url: link_url.clone(),
                });
                link_text.clear();
                link_url.clear();
            }
            // ── Text ──────────────────────────────────────────────────────
            pulldown_cmark::Event::Text(s) => {
                if let Some(ref mut buf) = code_buf {
                    buf.push_str(&s);
                } else if heading_level.is_some() {
                    heading_buf.push_str(&s);
                } else if in_link {
                    link_text.push_str(&s);
                } else {
                    events.push(Event::Text {
                        content: s.to_string(),
                        bold,
                        italic,
                        code_span: false,
                    });
                }
            }
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak
                if code_buf.is_none() && heading_level.is_none() && !in_link =>
            {
                events.push(Event::Text {
                    content: "\n".to_string(),
                    bold,
                    italic,
                    code_span: false,
                });
            }
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {}
            pulldown_cmark::Event::Start(Tag::BlockQuote(_)) => {}
            pulldown_cmark::Event::End(TagEnd::BlockQuote(_)) => {
                events.push(Event::Blank);
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
                .any(|e| matches!(e, Event::Heading { level: 1, text } if text == "Hello world")),
            "expected Heading(1, Hello world), got {evs:?}"
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
    fn bold_flag() {
        let evs = parse("**bold text**");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Text { bold: true, .. }))
        );
    }

    #[test]
    fn italic_flag() {
        let evs = parse("*italic*");
        assert!(
            evs.iter()
                .any(|e| matches!(e, Event::Text { italic: true, .. }))
        );
    }

    #[test]
    fn list_item_event() {
        let evs = parse("- item one\n- item two");
        let items: Vec<_> = evs
            .iter()
            .filter(|e| matches!(e, Event::ListItem { .. }))
            .collect();
        assert_eq!(items.len(), 2, "expected 2 ListItem events");
    }

    #[test]
    fn link_event() {
        let evs = parse("[click](https://hjkl.kryptic.sh)");
        assert!(evs.iter().any(|e| matches!(e, Event::Link { url, .. }
            if url == "https://hjkl.kryptic.sh")));
    }

    #[test]
    fn rule_event() {
        let evs = parse("---");
        assert!(evs.iter().any(|e| matches!(e, Event::Rule)));
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
}
