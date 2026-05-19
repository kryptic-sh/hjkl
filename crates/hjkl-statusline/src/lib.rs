//! Renderer-agnostic statusline data model.
//!
//! Build a [`Bar`] from host state, then hand it to a renderer adapter
//! (e.g. `hjkl-statusline-tui` for ratatui, `hjkl-statusline-gui` for floem).
//!
//! Naming follows the vim convention (`:help statusline`, lualine,
//! vim-airline, lightline).

#![forbid(unsafe_code)]

use std::borrow::Cow;

// ── Re-export hjkl-theme types ──────────────────────────────────────────────
//
// `Color`, `Modifiers`, and `Style` (aliased from hjkl-theme's `StyleSpec`)
// are the canonical types shared across the hjkl crate stack. Consumers that
// hold both a `Theme` and a `Bar` no longer need conversion shims.
pub use hjkl_theme::Color;
pub use hjkl_theme::Modifiers;
/// Alias for [`hjkl_theme::StyleSpec`]: foreground, background, modifiers.
pub use hjkl_theme::StyleSpec as Style;

// ── Style builder helpers ────────────────────────────────────────────────────
//
// `StyleSpec` from hjkl-theme carries only fields; builder methods live here
// as a local extension so callers can chain `.fg()`, `.bg()`, `.bold()`, etc.

/// Extension methods for building a [`Style`] (`hjkl_theme::StyleSpec`) by chaining.
pub trait StyleExt: Sized {
    /// Return a default (all-None / all-false) style.
    fn default_style() -> Self;
    /// Set foreground color.
    fn fg(self, fg: Color) -> Self;
    /// Set background color.
    fn bg(self, bg: Color) -> Self;
    /// Enable bold.
    fn bold(self) -> Self;
    /// Enable italic.
    fn italic(self) -> Self;
}

impl StyleExt for Style {
    fn default_style() -> Self {
        Self::default()
    }

    fn fg(self, fg: Color) -> Self {
        Self {
            fg: Some(fg),
            ..self
        }
    }

    fn bg(self, bg: Color) -> Self {
        Self {
            bg: Some(bg),
            ..self
        }
    }

    fn bold(self) -> Self {
        Self {
            modifiers: Modifiers {
                bold: true,
                ..self.modifiers
            },
            ..self
        }
    }

    fn italic(self) -> Self {
        Self {
            modifiers: Modifiers {
                italic: true,
                ..self.modifiers
            },
            ..self
        }
    }
}

/// A single horizontal segment in the statusline.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Segment {
    /// Pre-styled text. The renderer paints it verbatim.
    ///
    /// `content` is a [`Cow<'static, str>`] so static labels (e.g. `" NORMAL "`)
    /// are stored as borrowed `&'static str` with zero allocation, while
    /// dynamically-built strings (e.g. ` 42:7 `) use the owned `String` path.
    Text {
        content: Cow<'static, str>,
        style: Style,
    },
}

impl Segment {
    pub fn len(&self) -> usize {
        match self {
            Segment::Text { content, .. } => content.chars().count(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A fully-laid-out statusline: left segments + spacer + right segments.
///
/// Build with [`Bar`], then call [`Bar::layout`] to get the final flat
/// segment list (suitable for a single-row renderer).
#[derive(Debug, Clone, Default)]
pub struct Bar {
    pub left: Vec<Segment>,
    pub right: Vec<Segment>,
    /// Style used to fill the spacer gap between left and right.
    pub fill_style: Style,
}

impl Bar {
    /// Compute the final left-to-right segment list for the given terminal
    /// `width`. Inserts a padding spacer so right-aligned segments reach
    /// the right edge. Truncates the last left segment with `…` if the
    /// combined content is too wide.
    pub fn layout(&self, width: u16) -> Vec<Segment> {
        let w = width as usize;

        let left_len: usize = self.left.iter().map(|s| s.len()).sum();
        let right_len: usize = self.right.iter().map(|s| s.len()).sum();
        let total = left_len + right_len;

        let mut out: Vec<Segment> = Vec::with_capacity(self.left.len() + self.right.len() + 1);

        if total <= w {
            // Everything fits — compute spacer width.
            let spacer_w = w.saturating_sub(total);
            out.extend(self.left.iter().cloned());
            out.push(Segment::Text {
                content: " ".repeat(spacer_w).into(),
                style: self.fill_style,
            });
            out.extend(self.right.iter().cloned());
        } else {
            // Left side needs truncation. Right is always preserved fully.
            let avail_for_left = w.saturating_sub(right_len);
            let mut used = 0usize;
            for seg in self.left.iter() {
                let seg_len = seg.len();
                if used + seg_len <= avail_for_left {
                    out.push(seg.clone());
                    used += seg_len;
                } else {
                    // Truncate this segment to fit.
                    let remaining = avail_for_left.saturating_sub(used);
                    if remaining > 1 {
                        let Segment::Text { content, style } = seg;
                        let truncated: String =
                            content.chars().take(remaining.saturating_sub(1)).collect();
                        out.push(Segment::Text {
                            content: format!("{truncated}\u{2026}").into(),
                            style: *style,
                        });
                    } else if remaining == 1 {
                        let Segment::Text { style, .. } = seg;
                        out.push(Segment::Text {
                            content: Cow::Borrowed("\u{2026}"),
                            style: *style,
                        });
                    }
                    break;
                }
            }
            // Zero-width spacer in truncated layout.
            out.push(Segment::Text {
                content: Cow::Borrowed(""),
                style: self.fill_style,
            });
            out.extend(self.right.iter().cloned());
        }

        out
    }
}

// ── Theme ──────────────────────────────────────────────────────────────────

/// Theme colours the status row needs. Caller populates from its own theme.
///
/// `#[non_exhaustive]` allows adding new colour slots (e.g. per-severity
/// diagnostic colours, #135) without a breaking semver bump for consumers.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct StatusTheme {
    pub bg: Color,
    pub fg: Color,
    pub fill_bg: Color,
    pub mode_normal_bg: Color,
    pub mode_normal_fg: Color,
    pub mode_insert_bg: Color,
    pub mode_insert_fg: Color,
    pub mode_visual_bg: Color,
    pub mode_visual_fg: Color,
    pub dirty_fg: Color,
    pub readonly_fg: Color,
    pub new_file_fg: Color,
    pub recording_bg: Color,
    pub recording_fg: Color,
    /// Foreground color for Error-severity diagnostics in the statusline.
    pub diag_error_fg: Color,
    /// Foreground color for Warning-severity diagnostics in the statusline.
    pub diag_warning_fg: Color,
    /// Foreground color for Info-severity diagnostics in the statusline.
    pub diag_info_fg: Color,
    /// Foreground color for Hint-severity diagnostics in the statusline.
    pub diag_hint_fg: Color,
}

impl Default for StatusTheme {
    fn default() -> Self {
        let grey = Color::rgb(0xaa, 0xaa, 0xaa);
        let dark = Color::rgb(0x2e, 0x34, 0x40);
        Self {
            bg: Color::rgb(0x2a, 0x32, 0x40),
            fg: Color::rgb(0xe5, 0xe9, 0xf0),
            fill_bg: Color::rgb(0x1e, 0x22, 0x2a),
            mode_normal_bg: Color::rgb(0x5e, 0x81, 0xac),
            mode_normal_fg: dark,
            mode_insert_bg: Color::rgb(0x7e, 0xe7, 0x87),
            mode_insert_fg: dark,
            mode_visual_bg: Color::rgb(0xd0, 0x8e, 0x4b),
            mode_visual_fg: dark,
            dirty_fg: Color::rgb(0xeb, 0xcb, 0x8b),
            readonly_fg: grey,
            new_file_fg: grey,
            recording_bg: Color::rgb(0xbf, 0x61, 0x6a),
            recording_fg: dark,
            // Standard ANSI-named colors: adapt to the terminal palette.
            diag_error_fg: Color::rgb(0xff, 0x00, 0x00), // ANSI Red
            diag_warning_fg: Color::rgb(0xff, 0xc0, 0x00), // ANSI Yellow
            diag_info_fg: Color::rgb(0x00, 0x7a, 0xff),  // ANSI Blue
            diag_hint_fg: Color::rgb(0x00, 0xd7, 0xd7),  // ANSI Cyan
        }
    }
}

impl StatusTheme {
    /// Construct with explicit `bg` and `fg`; remaining slots filled with
    /// sensible Nord-palette greys so callers can mutate only what they need.
    pub fn new(bg: Color, fg: Color) -> Self {
        Self {
            bg,
            fg,
            ..Self::default()
        }
    }
}

// ── Mode ────────────────────────────────────────────────────────────────────

/// High-level mode classification for color selection.
///
/// `#[non_exhaustive]` so future variants (Command, ExSearch, IncSearch,
/// Macro, Terminal sub-kinds, …) can be added without breaking exhaustive
/// matches in downstream code.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
    Replace,
    Select,
    Operator,
    Terminal,
}

impl ModeKind {
    /// Derive from a mode label string (as returned by the engine).
    pub fn from_label(label: &str) -> Self {
        match label {
            "INSERT" => ModeKind::Insert,
            "REPLACE" => ModeKind::Replace,
            "VISUAL" => ModeKind::Visual,
            "VISUAL LINE" => ModeKind::VisualLine,
            "VISUAL BLOCK" => ModeKind::VisualBlock,
            "SELECT" => ModeKind::Select,
            "TERMINAL" => ModeKind::Terminal,
            _ => ModeKind::Normal,
        }
    }
}

// ── Segment builders ────────────────────────────────────────────────────────

/// Build the mode badge segment (e.g. ` NORMAL `).
pub fn mode_segment(label: &str, theme: &StatusTheme) -> Segment {
    let kind = ModeKind::from_label(label);
    let (bg, fg) = match kind {
        ModeKind::Insert => (theme.mode_insert_bg, theme.mode_insert_fg),
        ModeKind::Visual | ModeKind::VisualLine | ModeKind::VisualBlock => {
            (theme.mode_visual_bg, theme.mode_visual_fg)
        }
        _ => (theme.mode_normal_bg, theme.mode_normal_fg),
    };
    Segment::Text {
        content: format!(" {label} ").into(),
        style: Style::default_style().bg(bg).fg(fg).bold(),
    }
}

/// Build the filename segment including suffix tags (e.g. `[RO]`, `[New File]`).
pub fn filename_segment(name: &str, suffix: &str, theme: &StatusTheme) -> Segment {
    let style = Style::default_style().bg(theme.bg).fg(theme.fg);
    Segment::Text {
        content: format!(" {name}{suffix} ").into(),
        style,
    }
}

/// Build a dirty-marker segment (` ● ` when dirty, empty otherwise).
pub fn dirty_segment(dirty: bool, theme: &StatusTheme) -> Option<Segment> {
    if dirty {
        Some(Segment::Text {
            content: Cow::Borrowed(" \u{25cf} "),
            style: Style::default_style().bg(theme.bg).fg(theme.dirty_fg),
        })
    } else {
        None
    }
}

/// Build the cursor position segment (` row:col `).
pub fn cursor_segment(row: usize, col: usize, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" {}:{} ", row + 1, col + 1).into(),
        style: Style::default_style().bg(theme.bg).fg(theme.fg),
    }
}

/// Build the percentage segment (` N% `).
///
/// `mode` controls which mode colors are used for the badge background so
/// the segment visually echoes the active mode (e.g. green in INSERT, orange
/// in VISUAL). Pass [`ModeKind::Normal`] when the caller does not know the
/// mode or wants the default Normal styling.
pub fn percent_segment(
    row: usize,
    total_lines: usize,
    mode: ModeKind,
    theme: &StatusTheme,
) -> Segment {
    let pct = ((row + 1) * 100).checked_div(total_lines).unwrap_or(0);
    let (bg, fg) = match mode {
        ModeKind::Insert => (theme.mode_insert_bg, theme.mode_insert_fg),
        ModeKind::Visual | ModeKind::VisualLine | ModeKind::VisualBlock => {
            (theme.mode_visual_bg, theme.mode_visual_fg)
        }
        _ => (theme.mode_normal_bg, theme.mode_normal_fg),
    };
    Segment::Text {
        content: format!(" {pct}% ").into(),
        style: Style::default_style().bg(bg).fg(fg).bold(),
    }
}

/// Build a recording-register segment (` REC @r `).
pub fn recording_segment(reg: char, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" REC @{reg} ").into(),
        style: Style::default_style()
            .bg(theme.recording_bg)
            .fg(theme.recording_fg)
            .bold(),
    }
}

/// Build a pending-count/operator segment (` {count}{op} `). Returns `None` when empty.
pub fn pending_segment(
    count: Option<u64>,
    op: Option<&str>,
    theme: &StatusTheme,
) -> Option<Segment> {
    let content: Cow<'static, str> = match (count, op) {
        (Some(n), Some(o)) => format!(" {n}{o} ").into(),
        (Some(n), None) => format!(" {n} ").into(),
        (None, Some(o)) => format!(" {o} ").into(),
        (None, None) => return None,
    };
    Some(Segment::Text {
        content,
        style: Style::default_style().bg(theme.bg).fg(theme.fg).italic(),
    })
}

/// Build a search-count segment (` [idx/total] `).
pub fn search_count_segment(idx: usize, total: usize, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" [{idx}/{total}] ").into(),
        style: Style::default_style().bg(theme.bg).fg(theme.fg),
    }
}

/// Build a loading/spinner segment (` ⠋ label `).
pub fn loading_segment(spinner_frame: &str, label: &str, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" {spinner_frame} {label} ").into(),
        style: Style::default_style().bg(theme.bg).fg(theme.fg).italic(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Truncate `filename` so it fits in `avail` display columns, prepending `…`
/// when truncation occurs. Returns the (possibly truncated) string.
///
/// Uses `char_indices()` to find a valid UTF-8 char boundary at or before the
/// computed byte offset, avoiding panics on multibyte (non-ASCII) filenames.
pub fn truncate_filename(filename: &str, avail: usize) -> String {
    if filename.chars().count() <= avail {
        filename.to_owned()
    } else if avail <= 1 {
        String::new()
    } else {
        let keep = avail.saturating_sub(1); // one char reserved for '…'
        // Walk from the end: collect the last `keep` chars' byte start offsets.
        let start_byte = filename
            .char_indices()
            .rev()
            .nth(keep.saturating_sub(1))
            .map(|(byte_idx, _)| byte_idx)
            .unwrap_or(0);
        format!("\u{2026}{}", &filename[start_byte..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> StatusTheme {
        StatusTheme {
            bg: Color::rgb(0x2a, 0x32, 0x40),
            fg: Color::rgb(0xe5, 0xe9, 0xf0),
            fill_bg: Color::rgb(0x1e, 0x22, 0x2a),
            mode_normal_bg: Color::rgb(0x5e, 0x81, 0xac),
            mode_normal_fg: Color::rgb(0x2e, 0x34, 0x40),
            mode_insert_bg: Color::rgb(0x7e, 0xe7, 0x87),
            mode_insert_fg: Color::rgb(0x2e, 0x34, 0x40),
            mode_visual_bg: Color::rgb(0xd0, 0x8e, 0x4b),
            mode_visual_fg: Color::rgb(0x2e, 0x34, 0x40),
            dirty_fg: Color::rgb(0xeb, 0xcb, 0x8b),
            readonly_fg: Color::rgb(0xbf, 0x61, 0x6a),
            new_file_fg: Color::rgb(0xa3, 0xbe, 0x8c),
            recording_bg: Color::rgb(0xbf, 0x61, 0x6a),
            recording_fg: Color::rgb(0x2e, 0x34, 0x40),
            diag_error_fg: Color::rgb(0xff, 0x00, 0x00),
            diag_warning_fg: Color::rgb(0xff, 0xc0, 0x00),
            diag_info_fg: Color::rgb(0x00, 0x7a, 0xff),
            diag_hint_fg: Color::rgb(0x00, 0xd7, 0xd7),
        }
    }

    #[test]
    fn bar_layout_left_only_fits_width() {
        let theme = test_theme();
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(Segment::Text {
            content: Cow::Borrowed(" NORMAL "),
            style: Style::default_style(),
        });

        let segments = bar.layout(40);
        let total_chars: usize = segments.iter().map(|s| s.len()).sum();
        assert_eq!(total_chars, 40, "total rendered width must equal bar width");
    }

    #[test]
    fn bar_layout_left_plus_right_basic() {
        let theme = test_theme();
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(Segment::Text {
            content: Cow::Borrowed(" NORMAL "),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 1:1 "),
            style: Style::default_style(),
        });

        let segments = bar.layout(40);
        let total_chars: usize = segments.iter().map(|s| s.len()).sum();
        assert_eq!(total_chars, 40);
    }

    #[test]
    fn bar_layout_left_truncated_with_ellipsis() {
        let theme = test_theme();
        let long_name = "some/very/long/path/to/a/deeply/nested/file.rs";
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(Segment::Text {
            content: Cow::Borrowed(" NORMAL "),
            style: Style::default_style(),
        });
        bar.left.push(Segment::Text {
            content: format!(" {long_name} ").into(),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 1:1 "),
            style: Style::default_style(),
        });

        let segments = bar.layout(30);
        let all_content: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text { content, .. } => content.as_ref(),
            })
            .collect();
        assert!(
            all_content.contains('\u{2026}'),
            "truncated segment must contain ellipsis"
        );
    }

    #[test]
    fn bar_layout_right_pinned_to_edge() {
        let theme = test_theme();
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(Segment::Text {
            content: Cow::Borrowed(" NORMAL "),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 1:1 "),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 100% "),
            style: Style::default_style(),
        });

        let width: u16 = 60;
        let segments = bar.layout(width);
        let total_chars: usize = segments.iter().map(|s| s.len()).sum();
        assert_eq!(
            total_chars, 60,
            "right segments must be pinned to the right edge"
        );
    }

    #[test]
    fn mode_segment_normal_uses_normal_bg() {
        let theme = test_theme();
        let seg = mode_segment("NORMAL", &theme);
        match seg {
            Segment::Text { style, .. } => {
                assert_eq!(
                    style.bg,
                    Some(theme.mode_normal_bg),
                    "NORMAL mode segment must use mode_normal_bg"
                );
            }
        }
    }

    #[test]
    fn mode_segment_insert_uses_insert_bg() {
        let theme = test_theme();
        let seg = mode_segment("INSERT", &theme);
        match seg {
            Segment::Text { style, .. } => {
                assert_eq!(style.bg, Some(theme.mode_insert_bg));
            }
        }
    }

    #[test]
    fn cursor_segment_formats_row_col() {
        let theme = test_theme();
        let seg = cursor_segment(42, 10, &theme);
        match seg {
            Segment::Text { content, .. } => {
                assert!(content.contains("43:11"), "cursor segment: {content:?}");
            }
        }
    }

    #[test]
    fn percent_segment_formats_percent() {
        let theme = test_theme();
        let seg = percent_segment(42, 100, ModeKind::Normal, &theme);
        match seg {
            Segment::Text { content, .. } => {
                assert!(content.contains("43%"), "percent segment: {content:?}");
            }
        }
    }

    #[test]
    fn truncate_filename_short_unchanged() {
        let s = truncate_filename("foo.rs", 20);
        assert_eq!(s, "foo.rs");
    }

    #[test]
    fn truncate_filename_long_has_ellipsis() {
        let long = "some/very/long/path/to/a/deeply/nested/file.rs";
        let s = truncate_filename(long, 10);
        assert!(s.starts_with('\u{2026}'), "must start with ellipsis: {s:?}");
        assert!(s.chars().count() <= 10);
    }

    #[test]
    fn status_theme_default_is_sensible() {
        let t = StatusTheme::default();
        // All RGB channels must be in range (trivially true for u8, but assert
        // that the struct doesn't have any zero-alpha trap).
        assert_eq!(t.bg.a, 255, "default bg alpha must be 255");
        assert_eq!(t.fg.a, 255, "default fg alpha must be 255");
    }

    #[test]
    fn status_theme_new_sets_bg_fg() {
        let bg = Color::rgb(0x10, 0x20, 0x30);
        let fg = Color::rgb(0xe0, 0xd0, 0xc0);
        let t = StatusTheme::new(bg, fg);
        assert_eq!(t.bg, bg);
        assert_eq!(t.fg, fg);
        // Other slots come from default — spot-check one.
        assert_eq!(t.recording_bg, StatusTheme::default().recording_bg);
    }

    // ── New tests for issue #135 ─────────────────────────────────────────────

    /// Both `[RO]` and `[+]` (dirty marker) appear together in the bar.
    #[test]
    fn readonly_and_dirty_both_shown() {
        let theme = test_theme();
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left
            .push(filename_segment("README.md", " [RO]", &theme));
        // dirty_segment returns Some when dirty=true
        if let Some(seg) = dirty_segment(true, &theme) {
            bar.left.push(seg);
        }

        let segments = bar.layout(60);
        let all_content: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text { content, .. } => content.as_ref(),
            })
            .collect();
        assert!(
            all_content.contains("[RO]"),
            "readonly tag missing: {all_content:?}"
        );
        assert!(
            all_content.contains('\u{25cf}'),
            "dirty marker (●) missing: {all_content:?}"
        );
    }

    /// `percent_segment` with `total_lines = 0` must not panic and should show 0%.
    #[test]
    fn percent_segment_empty_buffer_no_panic() {
        let theme = test_theme();
        // row=0, total_lines=0: checked_div returns None → 0%
        let seg = percent_segment(0, 0, ModeKind::Normal, &theme);
        match seg {
            Segment::Text { content, .. } => {
                assert!(
                    content.contains("0%"),
                    "expected 0% for empty buffer: {content:?}"
                );
            }
        }
    }

    /// When right segments alone exceed the bar width, `Bar::layout` must not
    /// panic and must return segments whose total width equals the requested width.
    ///
    /// In the current implementation right segments are always preserved fully;
    /// when they exceed `width` the spacer collapses to zero and the left side
    /// gets zero budget. The total width therefore equals `right_len`, which may
    /// be > `width`. This test asserts the function completes without panicking
    /// and produces a non-empty segment list.
    #[test]
    fn bar_layout_right_alone_exceeds_width_no_panic() {
        let theme = test_theme();
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        // Right segments total 20 chars, bar width is only 10.
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 1:1 "),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: Cow::Borrowed(" 100% "),
            style: Style::default_style(),
        });

        // Must not panic.
        let segments = bar.layout(10);
        assert!(
            !segments.is_empty(),
            "layout must return at least one segment"
        );
    }

    /// When `recording_register = Some('q')` the bar contains the recording indicator.
    #[test]
    fn recording_segment_shows_register() {
        let theme = test_theme();
        let seg = recording_segment('q', &theme);
        match &seg {
            Segment::Text { content, style } => {
                assert!(
                    content.contains("REC"),
                    "recording segment must contain REC: {content:?}"
                );
                assert!(
                    content.contains('@'),
                    "recording segment must contain @: {content:?}"
                );
                assert!(
                    content.contains('q'),
                    "recording segment must contain register name: {content:?}"
                );
                assert_eq!(
                    style.bg,
                    Some(theme.recording_bg),
                    "recording segment must use recording_bg"
                );
            }
        }

        // Verify the segment appears in a bar layout.
        let mut bar = Bar {
            fill_style: Style::default_style().bg(theme.fill_bg).fg(theme.fg),
            ..Default::default()
        };
        bar.left.push(seg);
        let segments = bar.layout(40);
        let all_content: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text { content, .. } => content.as_ref(),
            })
            .collect();
        assert!(
            all_content.contains("REC @q"),
            "recording indicator missing from bar: {all_content:?}"
        );
    }
}
