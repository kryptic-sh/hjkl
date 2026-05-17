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

use hjkl_form::VimMode;
use hjkl_prompt::{PromptKind, PromptState};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

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

    let line = prompt_line_spans(&content, prompt.field.vim_mode(), theme, area.width);
    frame.render_widget(Paragraph::new(line), area);

    // Position terminal cursor at the insertion point.
    let (_, ccol) = prompt.field.cursor();
    let cursor_col = 1u16 + ccol as u16;
    frame.set_cursor_position((area.x + cursor_col, area.y));
}

/// Build a ratatui [`Line`] for the given prompt content string and mode.
///
/// Split into a body portion (padded) and a right-aligned `[I]`/`[N]` tag.
///
/// ```rust
/// use ratatui::style::Color;
/// use hjkl_form::VimMode;
/// use hjkl_prompt_tui::{PromptTheme, build_prompt_line};
///
/// let theme = PromptTheme::default();
/// let line = build_prompt_line(":write", VimMode::Insert, &theme, 40);
/// assert!(!line.spans.is_empty());
/// ```
pub fn build_prompt_line(
    content: &str,
    mode: VimMode,
    theme: &PromptTheme,
    width: u16,
) -> Line<'static> {
    prompt_line_spans(content, mode, theme, width)
}

fn prompt_line_spans(
    content: &str,
    mode: VimMode,
    theme: &PromptTheme,
    width: u16,
) -> Line<'static> {
    let (bg, tag, tag_fg) = match mode {
        VimMode::Insert => (theme.insert_bg, " [I]", theme.tag_insert_fg),
        _ => (theme.normal_bg, " [N]", theme.tag_normal_fg),
    };
    let body_width = (width as usize).saturating_sub(tag.len());
    let visible: String = content.chars().take(body_width).collect();
    let body = format!("{visible:<body_width$}");
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
        let entry_len = entry.chars().count();

        let remaining = n - i;
        let suffix = if remaining > 1 {
            format!("  +{} more", remaining - 1)
        } else {
            String::new()
        };

        if used + entry_len > width {
            if !suffix.is_empty() && used + suffix.len() <= width {
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
        let line = build_prompt_line(":write", VimMode::Insert, &theme, 40);
        assert!(!line.spans.is_empty());
    }

    #[test]
    fn build_prompt_line_contains_content() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":write", VimMode::Insert, &theme, 40);
        let all: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(all.contains(":write"), "content not found in: {all:?}");
    }

    #[test]
    fn build_prompt_line_insert_has_i_tag() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":w", VimMode::Insert, &theme, 20);
        let tags: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(tags.contains("[I]"), "expected [I] tag in: {tags:?}");
    }

    #[test]
    fn build_prompt_line_normal_has_n_tag() {
        let theme = PromptTheme::default();
        let line = build_prompt_line(":w", VimMode::Normal, &theme, 20);
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
