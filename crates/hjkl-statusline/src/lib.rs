//! Renderer-agnostic statusline data model.
//!
//! Build a [`Bar`] from host state, then hand it to a renderer adapter
//! (e.g. `hjkl-statusline-tui` for ratatui, `hjkl-statusline-gui` for floem).
//!
//! Naming follows the vim convention (`:help statusline`, lualine,
//! vim-airline, lightline).

#![forbid(unsafe_code)]

/// RGBA color (all channels 0–255, alpha ignored by TUI renderers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Text modifiers for a segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub bold: bool,
    pub italic: bool,
}

/// Pre-resolved style: foreground, background, and modifiers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Modifiers,
}

impl Style {
    pub const fn default_style() -> Self {
        Self {
            fg: None,
            bg: None,
            modifiers: Modifiers {
                bold: false,
                italic: false,
            },
        }
    }

    pub fn fg(self, fg: Color) -> Self {
        Self {
            fg: Some(fg),
            ..self
        }
    }

    pub fn bg(self, bg: Color) -> Self {
        Self {
            bg: Some(bg),
            ..self
        }
    }

    pub fn bold(self) -> Self {
        Self {
            modifiers: Modifiers {
                bold: true,
                ..self.modifiers
            },
            ..self
        }
    }

    pub fn italic(self) -> Self {
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
#[derive(Debug, Clone)]
pub enum Segment {
    /// Pre-styled text. The renderer paints it verbatim.
    Text { content: String, style: Style },
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
                content: " ".repeat(spacer_w),
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
                            content: format!("{truncated}\u{2026}"),
                            style: *style,
                        });
                    } else if remaining == 1 {
                        let Segment::Text { style, .. } = seg;
                        out.push(Segment::Text {
                            content: "\u{2026}".to_string(),
                            style: *style,
                        });
                    }
                    break;
                }
            }
            // Zero-width spacer in truncated layout.
            out.push(Segment::Text {
                content: String::new(),
                style: self.fill_style,
            });
            out.extend(self.right.iter().cloned());
        }

        out
    }
}

// ── Theme ──────────────────────────────────────────────────────────────────

/// Theme colours the status row needs. Caller populates from its own theme.
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
}

// ── Mode ────────────────────────────────────────────────────────────────────

/// High-level mode classification for color selection.
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
        content: format!(" {label} "),
        style: Style::default_style().bg(bg).fg(fg).bold(),
    }
}

/// Build the filename segment including suffix tags (e.g. `[RO]`, `[New File]`).
pub fn filename_segment(name: &str, suffix: &str, theme: &StatusTheme) -> Segment {
    let style = Style::default_style().bg(theme.bg).fg(theme.fg);
    Segment::Text {
        content: format!(" {name}{suffix} "),
        style,
    }
}

/// Build a dirty-marker segment (` ● ` when dirty, empty otherwise).
pub fn dirty_segment(dirty: bool, theme: &StatusTheme) -> Option<Segment> {
    if dirty {
        Some(Segment::Text {
            content: " \u{25cf} ".to_string(),
            style: Style::default_style().bg(theme.bg).fg(theme.dirty_fg),
        })
    } else {
        None
    }
}

/// Build the cursor position segment (` row:col `).
pub fn cursor_segment(row: usize, col: usize, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" {}:{} ", row + 1, col + 1),
        style: Style::default_style().bg(theme.bg).fg(theme.fg),
    }
}

/// Build the percentage segment (` N% `).
pub fn percent_segment(row: usize, total_lines: usize, theme: &StatusTheme) -> Segment {
    let pct = ((row + 1) * 100).checked_div(total_lines).unwrap_or(0);
    Segment::Text {
        content: format!(" {pct}% "),
        style: Style::default_style()
            .bg(theme.mode_normal_bg)
            .fg(theme.mode_normal_fg)
            .bold(),
    }
}

/// Build a recording-register segment (` REC @r `).
pub fn recording_segment(reg: char, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" REC @{reg} "),
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
    let content = match (count, op) {
        (Some(n), Some(o)) => format!(" {n}{o} "),
        (Some(n), None) => format!(" {n} "),
        (None, Some(o)) => format!(" {o} "),
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
        content: format!(" [{idx}/{total}] "),
        style: Style::default_style().bg(theme.bg).fg(theme.fg),
    }
}

/// Build a loading/spinner segment (` ⠋ label `).
pub fn loading_segment(spinner_frame: &str, label: &str, theme: &StatusTheme) -> Segment {
    Segment::Text {
        content: format!(" {spinner_frame} {label} "),
        style: Style::default_style().bg(theme.bg).fg(theme.fg).italic(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Truncate `filename` so it fits in `avail` display columns, prepending `…`
/// when truncation occurs. Returns the (possibly truncated) string.
pub fn truncate_filename(filename: &str, avail: usize) -> String {
    if filename.len() <= avail {
        filename.to_owned()
    } else if avail <= 1 {
        String::new()
    } else {
        let keep = avail.saturating_sub(1);
        let start = filename.len().saturating_sub(keep);
        format!("\u{2026}{}", &filename[start..])
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
            content: " NORMAL ".to_string(),
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
            content: " NORMAL ".to_string(),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: " 1:1 ".to_string(),
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
            content: " NORMAL ".to_string(),
            style: Style::default_style(),
        });
        bar.left.push(Segment::Text {
            content: format!(" {long_name} "),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: " 1:1 ".to_string(),
            style: Style::default_style(),
        });

        let segments = bar.layout(30);
        let all_content: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text { content, .. } => content.as_str(),
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
            content: " NORMAL ".to_string(),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: " 1:1 ".to_string(),
            style: Style::default_style(),
        });
        bar.right.push(Segment::Text {
            content: " 100% ".to_string(),
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
        let seg = percent_segment(42, 100, &theme);
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
}
