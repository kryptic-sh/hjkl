//! Ratatui adapter for `hjkl-prompt`.
//!
//! Renders a [`PromptState`] into a ratatui [`Frame`] as a bottom-bar status
//! row. The wildmenu strip is rendered as a separate row above the prompt when
//! completion is active.  Cursor shape is exposed via [`PromptState::cursor_shape`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! // (requires a real ratatui terminal — compile-checked, not run in CI)
//! use hjkl_prompt::{PromptState, PromptKind};
//! use hjkl_prompt_tui::{PromptTheme, render_prompt_line, render_wildmenu};
//! // frame and area come from your ratatui setup
//! ```

use hjkl_form::CoarseMode;
use hjkl_prompt::{PromptKind, PromptState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Compute a horizontally-scrolled window of `content` whose display width is
/// at most `body_width` cells and which keeps the char at index `cursor_char`
/// visible. Returns the window string and the cursor's display column within
/// that window. Walking back from the cursor is O(window), not O(content), so
/// a pathologically long prompt stays cheap.
fn scroll_window(content: &str, cursor_char: usize, body_width: usize) -> (String, usize) {
    if body_width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = content.chars().collect();
    let cur = cursor_char.min(chars.len());
    // Grow the window leftward from the cursor until the columns before it
    // reach `body_width - 1` (reserve one cell for the cursor itself).
    let reserve = body_width.saturating_sub(1);
    let mut start = cur;
    let mut cursor_col = 0usize;
    while start > 0 {
        let cw = UnicodeWidthChar::width(chars[start - 1]).unwrap_or(0);
        if cursor_col + cw > reserve {
            break;
        }
        cursor_col += cw;
        start -= 1;
    }
    // Collect forward from `start` until `body_width` columns are filled.
    let mut w = 0usize;
    let mut out = String::new();
    for &ch in &chars[start..] {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > body_width {
            break;
        }
        w += cw;
        out.push(ch);
    }
    (out, cursor_col)
}

// ── PromptTheme ───────────────────────────────────────────────────────────────

/// Theme slots for the prompt bar and wildmenu.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PromptTheme {
    /// Background colour for Insert-mode prompt.
    pub insert_bg: Color,
    /// Background colour for Normal-mode prompt.
    pub normal_bg: Color,
    /// Foreground colour for prompt text.
    pub text: Color,
    /// Foreground colour for the `[I]`/`[N]` mode tag in Insert mode.
    pub tag_insert_fg: Color,
    /// Foreground colour for the `[I]`/`[N]` mode tag in Normal mode.
    pub tag_normal_fg: Color,
    /// Background colour for the wildmenu strip.
    pub wildmenu_bg: Color,
    /// Foreground colour for unselected wildmenu entries.
    pub wildmenu_fg: Color,
    /// Background colour for the selected wildmenu entry.
    pub wildmenu_selection_bg: Color,
}

impl PromptTheme {
    /// Construct with explicit colours.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        insert_bg: Color,
        normal_bg: Color,
        text: Color,
        tag_insert_fg: Color,
        tag_normal_fg: Color,
        wildmenu_bg: Color,
        wildmenu_fg: Color,
        wildmenu_selection_bg: Color,
    ) -> Self {
        Self {
            insert_bg,
            normal_bg,
            text,
            tag_insert_fg,
            tag_normal_fg,
            wildmenu_bg,
            wildmenu_fg,
            wildmenu_selection_bg,
        }
    }
}

impl Default for PromptTheme {
    fn default() -> Self {
        Self {
            insert_bg: Color::Rgb(0x1e, 0x1e, 0x2e), // Catppuccin Mocha base
            normal_bg: Color::Rgb(0x31, 0x32, 0x44), // Catppuccin Mocha surface0
            text: Color::Rgb(0xcd, 0xd6, 0xf4),      // text
            tag_insert_fg: Color::Rgb(0xa6, 0xe3, 0xa1), // green
            tag_normal_fg: Color::Rgb(0xf3, 0x8b, 0xa8), // red
            wildmenu_bg: Color::Rgb(0x31, 0x32, 0x44),
            wildmenu_fg: Color::Rgb(0xcd, 0xd6, 0xf4),
            wildmenu_selection_bg: Color::Rgb(0x45, 0x47, 0x5a), // surface1
        }
    }
}

// ── render_prompt_line ────────────────────────────────────────────────────────

/// Render the prompt bar into `area` and set the terminal cursor position.
///
/// Renders `:text`, `/text`, or `?text` depending on the prompt kind.
/// A right-aligned `[I]`/`[N]` mode tag is appended.  The terminal cursor is
/// positioned at the text-insertion point via [`Frame::set_cursor_position`].
///
/// The caller is responsible for carving out `area` (typically the status-line
/// row at the bottom of the screen).
pub fn render_prompt_line(
    frame: &mut Frame,
    prompt: &PromptState,
    theme: &PromptTheme,
    area: Rect,
) {
    let prefix = prompt.kind.prefix_char();
    let text = prompt.field.text();
    let display: String = text.lines().next().unwrap_or("").to_string();
    let content = format!("{prefix}{display}");

    // Body columns available for text (the mode tag reserves the tail).
    let mode = prompt.field.coarse_mode();
    let tag_w = mode_tag(mode).width();
    let body_width = (area.width as usize).saturating_sub(tag_w);

    // Horizontal scroll: window the content around the cursor so long
    // prompts show the region being edited (not a stale head) and the
    // insertion point stays on screen. Cursor char index in `content` is
    // the single-column prefix plus the field's char column.
    let (_, ccol) = prompt.field.cursor();
    let cursor_char = ccol.saturating_add(1); // +1 for the prefix char
    let (visible, cursor_col) = scroll_window(&content, cursor_char, body_width);

    let line = prompt_line_spans(&visible, mode, theme, area.width);
    frame.render_widget(Paragraph::new(line), area);

    // Cursor display column within the visible window (already < body_width);
    // clamp to the bar for safety.
    let cx = cursor_col.min(area.width.saturating_sub(1) as usize) as u16;
    frame.set_cursor_position((area.x.saturating_add(cx), area.y));
}

/// Build a ratatui [`Line`] for the given prompt content string and mode.
///
/// Split into a body portion (padded) and a right-aligned `[I]`/`[N]` tag.
///
/// ```rust
/// use ratatui::style::Color;
/// use hjkl_form::CoarseMode;
/// use hjkl_prompt_tui::{PromptTheme, build_prompt_line};
///
/// let theme = PromptTheme::default();
/// let line = build_prompt_line(":write", CoarseMode::Insert, &theme, 40);
/// assert!(!line.spans.is_empty());
/// ```
pub fn build_prompt_line(
    content: &str,
    mode: CoarseMode,
    theme: &PromptTheme,
    width: u16,
) -> Line<'static> {
    prompt_line_spans(content, mode, theme, width)
}

/// The right-aligned mode tag string for the prompt bar.
fn mode_tag(mode: CoarseMode) -> &'static str {
    match mode {
        CoarseMode::Insert => " [I]",
        _ => " [N]",
    }
}

fn prompt_line_spans(
    content: &str,
    mode: CoarseMode,
    theme: &PromptTheme,
    width: u16,
) -> Line<'static> {
    let (bg, tag_fg) = match mode {
        CoarseMode::Insert => (theme.insert_bg, theme.tag_insert_fg),
        _ => (theme.normal_bg, theme.tag_normal_fg),
    };
    let tag = mode_tag(mode);
    let body_width = (width as usize).saturating_sub(tag.width());
    // Truncate to `body_width` display columns (wide chars count as 2), then
    // pad with spaces to fill the body so the background style extends fully.
    let mut w = 0usize;
    let mut visible = String::new();
    for ch in content.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > body_width {
            break;
        }
        w += cw;
        visible.push(ch);
    }
    let pad = body_width.saturating_sub(w);
    let body = format!("{visible}{}", " ".repeat(pad));
    Line::from(vec![
        Span::styled(body, Style::default().bg(bg).fg(theme.text)),
        Span::styled(tag.to_string(), Style::default().bg(bg).fg(tag_fg)),
    ])
}

// ── render_wildmenu ───────────────────────────────────────────────────────────

/// Render the wildmenu strip into `area`.
///
/// Shows all completion candidates in a single row; the selected one is
/// highlighted.  Candidates that don't fit are replaced with `+N more`.
/// Does nothing when `prompt.completion` is `None`.
pub fn render_wildmenu(frame: &mut Frame, prompt: &PromptState, theme: &PromptTheme, area: Rect) {
    let comp = match &prompt.completion {
        Some(c) => c,
        None => return,
    };

    let normal_style = Style::default().bg(theme.wildmenu_bg).fg(theme.wildmenu_fg);
    let selected_style = Style::default()
        .bg(theme.wildmenu_selection_bg)
        .fg(theme.wildmenu_fg);

    let width = area.width as usize;
    let sep = "  ";
    let sep_len = sep.len();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let n = comp.candidates.len();

    for (i, cand) in comp.candidates.iter().enumerate() {
        let is_selected = comp.selected == Some(i);
        let entry = cand.clone();
        let entry_len = UnicodeWidthStr::width(entry.as_str());

        // If we stop before rendering candidate `i`, candidates `i..n` are all
        // hidden — that is `n - i` of them (not `n - i - 1`).
        let hidden = n - i;
        let suffix = format!("  +{hidden} more");

        if used + entry_len > width {
            if used + suffix.len() <= width {
                spans.push(Span::styled(suffix, normal_style));
            }
            break;
        }

        if i > 0 {
            if used + sep_len + entry_len > width {
                if used + suffix.len() <= width {
                    spans.push(Span::styled(suffix, normal_style));
                }
                break;
            }
            spans.push(Span::styled(sep.to_string(), normal_style));
            used += sep_len;
        }

        let style = if is_selected {
            selected_style
        } else {
            normal_style
        };
        spans.push(Span::styled(entry, style));
        used += entry_len;
    }

    // Pad remainder.
    if used < width {
        let pad = " ".repeat(width - used);
        spans.push(Span::styled(pad, normal_style));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Returns `true` when the prompt has active wildmenu completion.
///
/// Convenience helper for callers that need to carve out an extra row for the
/// wildmenu before calling [`render_wildmenu`].
pub fn has_wildmenu(prompt: &PromptState) -> bool {
    prompt.completion.is_some()
}

/// Resolve the prompt kind from the `PromptKind` enum for callers that need to
/// check whether a forward/backward search is active.
///
/// ```rust
/// use hjkl_prompt::{PromptState, PromptKind};
/// use hjkl_prompt_tui::is_search_prompt;
///
/// let p = PromptState::new(PromptKind::SearchForward);
/// assert!(is_search_prompt(&p));
///
/// let p2 = PromptState::new(PromptKind::Command);
/// assert!(!is_search_prompt(&p2));
/// ```
pub fn is_search_prompt(prompt: &PromptState) -> bool {
    matches!(
        prompt.kind,
        PromptKind::SearchForward | PromptKind::SearchBackward
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_prompt::{CommandCompletion, PromptKind, PromptState};

    #[test]
    fn theme_default_constructs() {
        let t = PromptTheme::default();
        assert!(matches!(t.insert_bg, Color::Rgb(_, _, _)));
        assert!(matches!(t.wildmenu_bg, Color::Rgb(_, _, _)));
    }

    #[test]
    fn build_prompt_line_non_empty() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":write", CoarseMode::Insert, &theme, 40);
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn build_prompt_line_contains_content() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":write", CoarseMode::Insert, &theme, 40);
        let all: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all.contains(":write"), "content not found in: {all:?}");
    }

    #[test]
    fn build_prompt_line_insert_has_i_tag() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":w", CoarseMode::Insert, &theme, 20);
        let tags: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(tags.contains("[I]"), "expected [I] tag in: {tags:?}");
    }

    #[test]
    fn build_prompt_line_normal_has_n_tag() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":w", CoarseMode::Normal, &theme, 20);
        let tags: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(tags.contains("[N]"), "expected [N] tag in: {tags:?}");
    }

    #[test]
    fn has_wildmenu_false_when_no_completion() {
        let p = PromptState::new(PromptKind::Command);
        assert!(!has_wildmenu(&p));
    }

    #[test]
    fn has_wildmenu_true_when_completion_set() {
        let mut p = PromptState::new(PromptKind::Command);
        p.completion = Some(CommandCompletion::new(
            "w".into(),
            vec!["write".into(), "wall".into()],
            0..1,
        ));
        assert!(has_wildmenu(&p));
    }

    #[test]
    fn wildmenu_counts_hidden_last_candidate() {
        use ratatui::{Terminal, backend::TestBackend};

        // Regression: when the candidate that fails to fit was the LAST one,
        // the strip used to render no "+N more" indicator at all (and always
        // under-counted the hidden candidates by one).
        let mut p = PromptState::new(PromptKind::Command);
        p.completion = Some(CommandCompletion::new(
            "e".into(),
            vec!["short".into(), "averyverylongcandidatename".into()],
            0..1,
        ));
        let theme = PromptTheme::default();

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 20, 1);
                render_wildmenu(frame, &p, &theme, area);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let all: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            all.contains("+1 more"),
            "hidden last candidate must be counted: {all:?}"
        );
    }

    #[test]
    fn render_prompt_line_huge_input_does_not_panic() {
        use ratatui::{Terminal, backend::TestBackend};

        // Regression: a 65535-char prompt line used to overflow the
        // `1u16 + ccol as u16` cursor math in debug builds.
        let mut p = PromptState::new(PromptKind::Command);
        p.apply_history_nav(&["x".repeat(65_535)], Some(0));
        let theme = PromptTheme::default();

        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_prompt_line(frame, &p, &theme, Rect::new(0, 0, 80, 1));
            })
            .unwrap();
    }

    #[test]
    fn scroll_window_short_content_not_scrolled() {
        let (vis, col) = scroll_window(":write", 6, 40);
        assert_eq!(vis, ":write");
        assert_eq!(col, 6);
    }

    #[test]
    fn scroll_window_long_input_keeps_cursor_visible() {
        // 78 chars, cursor near the end, 20-col body.
        let content: String = "abcdefghijklmnopqrstuvwxyz".repeat(3);
        let (vis, col) = scroll_window(&content, 70, 20);
        assert!(
            UnicodeWidthStr::width(vis.as_str()) <= 20,
            "window overflows body: {vis:?}"
        );
        assert!(col < 20, "cursor col {col} must stay within body");
        // The char just before the cursor must be inside the window.
        let before = content.chars().nth(69).unwrap();
        assert!(vis.contains(before), "cursor region not visible: {vis:?}");
    }

    #[test]
    fn scroll_window_wide_chars_stay_within_width() {
        // 20 double-width CJK chars = 40 cols; cursor at end, 20-col body.
        let content: String = "世".repeat(20);
        let (vis, col) = scroll_window(&content, 20, 20);
        assert!(UnicodeWidthStr::width(vis.as_str()) <= 20);
        assert!(col < 20, "cursor col {col} must stay within body");
    }

    #[test]
    fn is_search_prompt_forward() {
        let p = PromptState::new(PromptKind::SearchForward);
        assert!(is_search_prompt(&p));
    }

    #[test]
    fn is_search_prompt_backward() {
        let p = PromptState::new(PromptKind::SearchBackward);
        assert!(is_search_prompt(&p));
    }

    #[test]
    fn is_search_prompt_command_false() {
        let p = PromptState::new(PromptKind::Command);
        assert!(!is_search_prompt(&p));
    }
}
