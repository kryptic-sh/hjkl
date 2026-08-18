//! Ratatui adapter for `hjkl-hover`.
//!
//! Paints a [`HoverState`] into a ratatui [`Frame`] using
//! `hjkl-markdown-tui` for the content body. The popup is a floating
//! bordered box whose position is computed by [`hjkl_hover::position`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_hover::{HoverAnchor, HoverState, HoverViewport};
//! use hjkl_hover_tui::{HoverTheme, render};
//! // frame and viewport come from your ratatui setup
//! ```

use hjkl_hover::{HoverState, HoverViewport, position};
use hjkl_markdown::parse;
use hjkl_markdown_tui::{MdTheme, to_lines};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

// ── HoverTheme ────────────────────────────────────────────────────────────────

/// Theme slots for the hover popup chrome (border, title, background) plus the
/// markdown body colors.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HoverTheme {
    /// Border and title foreground.
    pub border: ratatui::style::Color,
    /// Popup background.
    pub background: ratatui::style::Color,
    /// Markdown body colors.
    pub md: MdTheme,
}

impl HoverTheme {
    /// Construct from explicit values.
    pub fn new(
        border: ratatui::style::Color,
        background: ratatui::style::Color,
        md: MdTheme,
    ) -> Self {
        Self {
            border,
            background,
            md,
        }
    }
}

impl Default for HoverTheme {
    fn default() -> Self {
        Self {
            border: ratatui::style::Color::Rgb(0x89, 0xb4, 0xfa),
            background: ratatui::style::Color::Rgb(0x1e, 0x1e, 0x2e),
            md: MdTheme::default(),
        }
    }
}

// ── HoverRenderCache ───────────────────────────────────────────────────────────

/// Per-popup render cache: parsed markdown (content-keyed — the parse is
/// width-independent) and width-wrapped lines (keyed on content + width, so
/// only a terminal resize re-wraps). Held by the caller (the app) and
/// threaded through [`render`]; self-invalidates across popup replacements
/// because both entries are keyed on the content string.
pub struct HoverRenderCache {
    /// (content, parsed events) — re-parse only when content changes.
    parsed: Option<(String, Vec<hjkl_markdown::Event>)>,
    /// (content, width, wrapped lines) — re-wrap only when content or width
    /// changes. The lines embed the theme colours; the app builds a constant
    /// `MdTheme::default()` per frame, so the theme is not part of the key.
    wrapped: Option<(String, u16, Vec<ratatui::text::Line<'static>>)>,
    /// Parse count; tests read this to prove a repeat draw hits the cache.
    pub parses: u64,
    /// Re-wrap count; tests read this to prove a width change re-wraps.
    pub wraps: u64,
}

impl HoverRenderCache {
    /// New empty cache.
    pub fn new() -> Self {
        Self {
            parsed: None,
            wrapped: None,
            parses: 0,
            wraps: 0,
        }
    }
}
impl Default for HoverRenderCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── render ────────────────────────────────────────────────────────────────────

/// Render a hover popup into `frame`.
///
/// `viewport` is the full terminal area available (typically `frame.area()`).
/// The popup position is computed by [`hjkl_hover::position`], then the
/// markdown content is parsed and rendered via `hjkl-markdown-tui`.
///
/// `cache` holds the parsed events and wrapped lines between frames, so a
/// steady repaint while the popup is visible skips the markdown parse (only
/// re-run when `state.content` changes) and the width wrap (only re-run when
/// content or `inner.width` changes).
pub fn render(
    frame: &mut Frame,
    state: &HoverState,
    theme: &HoverTheme,
    viewport: Rect,
    cache: &mut HoverRenderCache,
) {
    let vp = HoverViewport::new(viewport.width, viewport.height);
    let hr = position(state, vp);
    let rect = Rect {
        x: viewport.x + hr.x,
        y: viewport.y + hr.y,
        width: hr.width,
        height: hr.height,
    };

    frame.render_widget(Clear, rect);

    let border_style = Style::default().fg(theme.border);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" hover ");
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Parse + render markdown into the inner area. Both steps are cached:
    // re-parse only when the content changes (the parse is width-independent),
    // re-wrap only when content or width changes. Disjoint-field borrows let
    // `events` (borrowed from `cache.parsed`) coexist with mutating
    // `cache.wrapped`.
    if cache
        .parsed
        .as_ref()
        .is_none_or(|(c, _)| c != &state.content)
    {
        cache.parsed = Some((state.content.clone(), parse(&state.content)));
        cache.parses += 1;
    }
    let events: &[hjkl_markdown::Event] = &cache.parsed.as_ref().unwrap().1;

    if cache
        .wrapped
        .as_ref()
        .is_none_or(|(c, w, _)| c != &state.content || *w != inner.width)
    {
        cache.wrapped = Some((
            state.content.clone(),
            inner.width,
            to_lines(events, &theme.md, inner.width),
        ));
        cache.wraps += 1;
    }
    let lines: Vec<ratatui::text::Line<'static>> = cache.wrapped.as_ref().unwrap().2.clone();

    // Apply scroll offset, clamped so scrolling past the end never blanks
    // the popup (HoverState::scroll_lines has no upper bound; the renderer
    // is responsible for enforcing it).
    let max_scroll = lines.len().saturating_sub(inner.height.max(1) as usize);
    let scrolled: Vec<_> = lines
        .into_iter()
        .skip(state.scroll.min(max_scroll))
        .collect();

    let para = Paragraph::new(scrolled)
        .style(Style::default().fg(theme.md.text).bg(theme.background))
        .wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_hover::{HoverAnchor, HoverState, HoverViewport};

    fn make_state(content: &str, col: u16, row: u16) -> HoverState {
        HoverState::new(content.to_string(), HoverAnchor::new(col, row))
    }

    #[test]
    fn hover_theme_default_has_border() {
        let t = HoverTheme::default();
        assert!(matches!(t.border, ratatui::style::Color::Rgb(_, _, _)));
    }

    #[test]
    fn position_smoke() {
        let s = make_state("hello", 5, 5);
        let r = hjkl_hover::position(&s, HoverViewport::new(80, 24));
        assert!(r.x + r.width <= 80);
        assert!(r.y + r.height <= 24);
    }

    #[test]
    fn scroll_integration() {
        let mut s = make_state("line1\nline2\nline3", 0, 0);
        s.scroll_lines(1);
        assert_eq!(s.scroll, 1);
        let evs = parse(&s.content);
        let lines = to_lines(&evs, &MdTheme::default(), 80);
        let scrolled: Vec<_> = lines.into_iter().skip(s.scroll).collect();
        // Should have 2 or fewer lines after skipping 1.
        assert!(scrolled.len() <= 3, "unexpected line count");
    }

    #[test]
    fn overscrolled_popup_still_shows_content() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut s = make_state("alpha\nbravo\ncharlie", 0, 0);
        s.scroll_lines(10_000); // far past the end
        let theme = HoverTheme::default();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cache = HoverRenderCache::new();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, &s, &theme, area, &mut cache);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let all: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            all.contains("charlie"),
            "overscrolled popup must clamp and keep the tail visible"
        );
    }

    #[test]
    fn markdown_parsed_in_hover() {
        let s = make_state("# Title\n\nhello `world`", 0, 0);
        let evs = parse(&s.content);
        let lines = to_lines(&evs, &MdTheme::default(), 60);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|sp| sp.content.as_ref())
            .collect();
        assert!(
            all_text.contains("Title"),
            "heading not found: {all_text:?}"
        );
    }

    #[test]
    fn render_cache_counts_parse_and_wrap() {
        use ratatui::{Terminal, backend::TestBackend};

        // Content with a line wide enough that the popup width tracks the
        // viewport (max_width is 62, so a >60-col line makes inner.width
        // differ between an 80- and 60-col viewport).
        let mut s = make_state(
            &format!("# Title\n\nhello `code` {}", "longword ".repeat(12)),
            0,
            0,
        );
        let theme = HoverTheme::default();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cache = HoverRenderCache::new();
        let draw = |terminal: &mut Terminal<TestBackend>,
                    cache: &mut HoverRenderCache,
                    s: &HoverState,
                    viewport: Rect| {
            terminal
                .draw(|frame| render(frame, s, &theme, viewport, cache))
                .unwrap();
        };

        // 1. First draw parses and wraps exactly once.
        draw(&mut terminal, &mut cache, &s, Rect::new(0, 0, 80, 24));
        assert_eq!(
            (cache.parses, cache.wraps),
            (1, 1),
            "first draw must parse and wrap once"
        );

        // 2. Repeat draw, unchanged state + viewport → both cache hits.
        draw(&mut terminal, &mut cache, &s, Rect::new(0, 0, 80, 24));
        assert_eq!(
            (cache.parses, cache.wraps),
            (1, 1),
            "repeat draw must hit both caches"
        );

        // 3. Narrower viewport → re-wrap only (the parse is width-independent).
        draw(&mut terminal, &mut cache, &s, Rect::new(0, 0, 60, 24));
        assert_eq!(cache.parses, 1, "width change must not re-parse");
        assert_eq!(cache.wraps, 2, "width change must re-wrap");

        // 4. New content → re-parse + re-wrap, and the buffer shows it.
        s = make_state(
            &format!("# New\n\nbye `code` {}", "different ".repeat(12)),
            0,
            0,
        );
        draw(&mut terminal, &mut cache, &s, Rect::new(0, 0, 80, 24));
        assert_eq!(cache.parses, 2, "content change must re-parse");
        assert_eq!(cache.wraps, 3, "content change must re-wrap");

        let buf = terminal.backend().buffer().clone();
        let all: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            all.contains("New"),
            "popup must show the new content: {all:?}"
        );
        assert!(
            !all.contains("Title"),
            "stale content must be gone from the buffer: {all:?}"
        );
    }
}
