//! Floem adapter for `hjkl-prompt`.
//!
//! Renders a [`PromptState`] as a floem view: a single-line prompt bar
//! (`:text`, `/text`, or `?text`) with the wildmenu completion strip shown
//! above it while completion is active. Mirrors the structure of the
//! ratatui adapter `hjkl-prompt-tui` — a thin [`View`](floem::View) builder
//! ([`prompt_view`]) delegates all display logic to pure, unit-tested helper
//! functions ([`prompt_display_text`], [`prompt_cursor_col`],
//! [`split_at_cursor`], [`wildmenu_items`]) so the logic can be tested
//! without a running floem application.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use floem::reactive::RwSignal;
//! use hjkl_prompt::{PromptState, PromptKind};
//! use hjkl_prompt_gui::{PromptThemeGui, prompt_view};
//!
//! let state = RwSignal::new(PromptState::new(PromptKind::Command));
//! let _view = prompt_view(state, PromptThemeGui::default());
//! ```

use floem::{
    IntoView,
    peniko::Color,
    reactive::{RwSignal, SignalWith},
    views::{Decorators, dyn_stack, label, v_stack},
};
use hjkl_prompt::{PromptKind, PromptState};

// ── PromptThemeGui ────────────────────────────────────────────────────────────

/// Theme slots for the prompt bar and wildmenu.
///
/// `#[non_exhaustive]` — new slots may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PromptThemeGui {
    /// Background colour for the prompt bar.
    pub bg: Color,
    /// Foreground colour for prompt text.
    pub fg: Color,
    /// Background colour for unselected wildmenu entries.
    pub wildmenu_bg: Color,
    /// Foreground colour for wildmenu entries (selected and unselected).
    pub wildmenu_fg: Color,
    /// Background colour for the selected wildmenu entry.
    pub wildmenu_selected_bg: Color,
}

impl PromptThemeGui {
    /// Construct with explicit colours.
    pub fn new(
        bg: Color,
        fg: Color,
        wildmenu_bg: Color,
        wildmenu_fg: Color,
        wildmenu_selected_bg: Color,
    ) -> Self {
        Self {
            bg,
            fg,
            wildmenu_bg,
            wildmenu_fg,
            wildmenu_selected_bg,
        }
    }
}

impl Default for PromptThemeGui {
    fn default() -> Self {
        Self {
            bg: Color::rgb8(0x1e, 0x1e, 0x2e),          // Catppuccin Mocha base
            fg: Color::rgb8(0xcd, 0xd6, 0xf4),          // text
            wildmenu_bg: Color::rgb8(0x31, 0x32, 0x44), // surface0
            wildmenu_fg: Color::rgb8(0xcd, 0xd6, 0xf4), // text
            wildmenu_selected_bg: Color::rgb8(0x45, 0x47, 0x5a), // surface1
        }
    }
}

// ── Pure display-logic helpers ────────────────────────────────────────────────

/// Compute the displayed prompt line: the prefix char (`:`, `/`, `?`)
/// followed by the field's text, truncated to its first line.
///
/// ```rust
/// use hjkl_prompt::{PromptState, PromptKind};
/// use hjkl_prompt_gui::prompt_display_text;
///
/// let p = PromptState::with_prefill(PromptKind::Command, "write");
/// assert_eq!(prompt_display_text(&p), ":write");
/// ```
pub fn prompt_display_text(state: &PromptState) -> String {
    let prefix = state.kind.prefix_char();
    let text = state.text();
    let display = text.lines().next().unwrap_or("");
    format!("{prefix}{display}")
}

/// Compute the cursor's char column within [`prompt_display_text`]'s output.
///
/// The field's own cursor column is offset by one to account for the leading
/// prefix char.
///
/// ```rust
/// use hjkl_prompt::{PromptState, PromptKind};
/// use hjkl_prompt_gui::prompt_cursor_col;
///
/// let p = PromptState::with_prefill(PromptKind::Command, "write");
/// // Cursor sits after "write" (5 chars) plus the prefix char.
/// assert_eq!(prompt_cursor_col(&p), 6);
/// ```
pub fn prompt_cursor_col(state: &PromptState) -> usize {
    let (_, ccol) = state.cursor();
    ccol.saturating_add(1)
}

/// Split `text` into `(before, at, after)` around the char at `cursor_col`.
///
/// `at` is the single char at `cursor_col`, or an empty string when the
/// cursor sits at or past the end of `text` (nothing to highlight there).
///
/// ```rust
/// use hjkl_prompt_gui::split_at_cursor;
///
/// let (before, at, after) = split_at_cursor(":write", 1);
/// assert_eq!(before, ":");
/// assert_eq!(at, "w");
/// assert_eq!(after, "rite");
///
/// // Cursor past the end: nothing to highlight.
/// let (before, at, after) = split_at_cursor(":write", 6);
/// assert_eq!(before, ":write");
/// assert_eq!(at, "");
/// assert_eq!(after, "");
/// ```
pub fn split_at_cursor(text: &str, cursor_col: usize) -> (String, String, String) {
    let chars: Vec<char> = text.chars().collect();
    let col = cursor_col.min(chars.len());
    let before: String = chars[..col].iter().collect();
    let at: String = chars.get(col).map(|c| c.to_string()).unwrap_or_default();
    let after: String = if col < chars.len() {
        chars[col + 1..].iter().collect()
    } else {
        String::new()
    };
    (before, at, after)
}

/// A single wildmenu entry as displayed by [`prompt_view`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WildmenuItem {
    /// The candidate text.
    pub text: String,
    /// Whether this entry is the currently-selected candidate.
    pub selected: bool,
}

/// Compute the wildmenu's visible items and which one is selected.
///
/// Returns an empty `Vec` when no completion is active.
///
/// ```rust
/// use hjkl_prompt::{CommandCompletion, PromptKind, PromptState};
/// use hjkl_prompt_gui::wildmenu_items;
///
/// let mut p = PromptState::new(PromptKind::Command);
/// assert!(wildmenu_items(&p).is_empty());
///
/// p.completion = Some(CommandCompletion::new(
///     "w".into(),
///     vec!["write".into(), "wall".into()],
///     0..1,
/// ));
/// let items = wildmenu_items(&p);
/// assert_eq!(items.len(), 2);
/// assert!(!items[0].selected);
/// ```
pub fn wildmenu_items(state: &PromptState) -> Vec<WildmenuItem> {
    match &state.completion {
        Some(comp) => comp
            .candidates
            .iter()
            .enumerate()
            .map(|(i, cand)| WildmenuItem {
                text: cand.clone(),
                selected: comp.selected == Some(i),
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Returns `true` when the prompt has active wildmenu completion.
///
/// Convenience helper for callers that need to decide whether to reserve
/// space for the wildmenu strip before calling [`prompt_view`].
///
/// ```rust
/// use hjkl_prompt::{PromptKind, PromptState};
/// use hjkl_prompt_gui::has_wildmenu;
///
/// let p = PromptState::new(PromptKind::Command);
/// assert!(!has_wildmenu(&p));
/// ```
pub fn has_wildmenu(state: &PromptState) -> bool {
    state.completion.is_some()
}

/// Resolve whether `state` is a forward/backward search prompt.
///
/// ```rust
/// use hjkl_prompt::{PromptKind, PromptState};
/// use hjkl_prompt_gui::is_search_prompt;
///
/// let p = PromptState::new(PromptKind::SearchForward);
/// assert!(is_search_prompt(&p));
///
/// let p2 = PromptState::new(PromptKind::Command);
/// assert!(!is_search_prompt(&p2));
/// ```
pub fn is_search_prompt(state: &PromptState) -> bool {
    matches!(
        state.kind,
        PromptKind::SearchForward | PromptKind::SearchBackward
    )
}

// ── prompt_view ───────────────────────────────────────────────────────────────

/// Build a floem view for the prompt bar, including the wildmenu strip.
///
/// `state` is a reactive signal owned by the caller (typically bumped on
/// every keystroke handled by the prompt's `TextFieldEditor`). The wildmenu
/// row renders no items (i.e. takes no visible space) when completion is
/// inactive.
///
/// The view itself contains no display logic — it renders whatever
/// [`prompt_display_text`], [`split_at_cursor`], and [`wildmenu_items`]
/// compute, so all logic worth testing lives in those pure functions.
pub fn prompt_view(state: RwSignal<PromptState>, theme: PromptThemeGui) -> impl IntoView {
    let line_theme = theme.clone();
    let line = label(move || state.with(prompt_display_text)).style(move |s| {
        s.width_full()
            .font_family("monospace".to_string())
            .color(line_theme.fg)
            .background(line_theme.bg)
    });

    let wildmenu = dyn_stack(
        move || state.with(wildmenu_items),
        |item: &WildmenuItem| item.text.clone(),
        move |item: WildmenuItem| {
            let bg = if item.selected {
                theme.wildmenu_selected_bg
            } else {
                theme.wildmenu_bg
            };
            let fg = theme.wildmenu_fg;
            label(move || item.text.clone())
                .style(move |s| s.padding_horiz(6.0).color(fg).background(bg))
        },
    )
    .style(|s| s.width_full());

    v_stack((wildmenu, line)).style(|s| s.width_full())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_prompt::CommandCompletion;

    // ── prompt_display_text ──────────────────────────────────────────────

    #[test]
    fn display_text_command_prefix() {
        let p = PromptState::new(PromptKind::Command);
        assert_eq!(prompt_display_text(&p), ":");
    }

    #[test]
    fn display_text_search_forward_prefix() {
        let p = PromptState::new(PromptKind::SearchForward);
        assert_eq!(prompt_display_text(&p), "/");
    }

    #[test]
    fn display_text_search_backward_prefix() {
        let p = PromptState::new(PromptKind::SearchBackward);
        assert_eq!(prompt_display_text(&p), "?");
    }

    #[test]
    fn display_text_empty_is_just_prefix() {
        let p = PromptState::new(PromptKind::Command);
        assert_eq!(prompt_display_text(&p), ":");
    }

    #[test]
    fn display_text_non_empty_includes_content() {
        let p = PromptState::with_prefill(PromptKind::Command, "write");
        assert_eq!(prompt_display_text(&p), ":write");
    }

    #[test]
    fn display_text_search_non_empty_includes_content() {
        let p = PromptState::with_prefill(PromptKind::SearchForward, "needle");
        assert_eq!(prompt_display_text(&p), "/needle");
    }

    #[test]
    fn display_text_only_first_line() {
        let mut p = PromptState::new(PromptKind::Command);
        p.apply_history_nav(&["line1\nline2".to_string()], Some(0));
        assert_eq!(prompt_display_text(&p), ":line1");
    }

    // ── prompt_cursor_col ─────────────────────────────────────────────────

    #[test]
    fn cursor_col_empty_prompt_is_one() {
        let p = PromptState::new(PromptKind::Command);
        assert_eq!(prompt_cursor_col(&p), 1);
    }

    #[test]
    fn cursor_col_after_prefill_is_len_plus_one() {
        let p = PromptState::with_prefill(PromptKind::Command, "write");
        assert_eq!(prompt_cursor_col(&p), 6);
    }

    // ── split_at_cursor ───────────────────────────────────────────────────

    #[test]
    fn split_at_cursor_middle() {
        let (before, at, after) = split_at_cursor(":write", 1);
        assert_eq!(before, ":");
        assert_eq!(at, "w");
        assert_eq!(after, "rite");
    }

    #[test]
    fn split_at_cursor_start() {
        let (before, at, after) = split_at_cursor(":write", 0);
        assert_eq!(before, "");
        assert_eq!(at, ":");
        assert_eq!(after, "write");
    }

    #[test]
    fn split_at_cursor_past_end_has_no_highlight() {
        let (before, at, after) = split_at_cursor(":write", 6);
        assert_eq!(before, ":write");
        assert_eq!(at, "");
        assert_eq!(after, "");
    }

    #[test]
    fn split_at_cursor_way_past_end_clamps() {
        let (before, at, after) = split_at_cursor(":write", 999);
        assert_eq!(before, ":write");
        assert_eq!(at, "");
        assert_eq!(after, "");
    }

    #[test]
    fn split_at_cursor_empty_text() {
        let (before, at, after) = split_at_cursor("", 0);
        assert_eq!(before, "");
        assert_eq!(at, "");
        assert_eq!(after, "");
    }

    // ── wildmenu_items / has_wildmenu ─────────────────────────────────────

    #[test]
    fn wildmenu_items_empty_when_no_completion() {
        let p = PromptState::new(PromptKind::Command);
        assert!(wildmenu_items(&p).is_empty());
    }

    #[test]
    fn wildmenu_items_lists_candidates() {
        let mut p = PromptState::new(PromptKind::Command);
        p.completion = Some(CommandCompletion::new(
            "w".into(),
            vec!["write".into(), "wall".into()],
            0..1,
        ));
        let items = wildmenu_items(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "write");
        assert_eq!(items[1].text, "wall");
    }

    #[test]
    fn wildmenu_items_marks_selected_candidate() {
        let mut p = PromptState::new(PromptKind::Command);
        let comp = CommandCompletion::new("w".into(), vec!["write".into(), "wall".into()], 0..1);
        p.advance_completion(Some(comp), true);
        let items = wildmenu_items(&p);
        assert!(items[0].selected);
        assert!(!items[1].selected);
    }

    #[test]
    fn wildmenu_items_none_selected_before_first_advance() {
        let mut p = PromptState::new(PromptKind::Command);
        p.completion = Some(CommandCompletion::new(
            "w".into(),
            vec!["write".into(), "wall".into()],
            0..1,
        ));
        let items = wildmenu_items(&p);
        assert!(items.iter().all(|i| !i.selected));
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

    // ── is_search_prompt ──────────────────────────────────────────────────

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

    // ── PromptThemeGui ────────────────────────────────────────────────────

    #[test]
    fn theme_default_constructs() {
        let t = PromptThemeGui::default();
        assert_eq!(t.fg, Color::rgb8(0xcd, 0xd6, 0xf4));
    }

    #[test]
    fn theme_new_sets_fields() {
        let t = PromptThemeGui::new(
            Color::rgb8(1, 2, 3),
            Color::rgb8(4, 5, 6),
            Color::rgb8(7, 8, 9),
            Color::rgb8(10, 11, 12),
            Color::rgb8(13, 14, 15),
        );
        assert_eq!(t.bg, Color::rgb8(1, 2, 3));
        assert_eq!(t.wildmenu_selected_bg, Color::rgb8(13, 14, 15));
    }

    // ── prompt_view (smoke: must construct without panicking) ────────────

    #[test]
    fn prompt_view_constructs() {
        let state = RwSignal::new(PromptState::new(PromptKind::Command));
        let _view = prompt_view(state, PromptThemeGui::default());
    }
}
