//! Renderer-agnostic info popup data model.
//!
//! Provides [`InfoPopup`] — the state for a centered floating overlay used by
//! `:reg`, `:marks`, `:jumps`, `:changes`, and the K-key LSP hover info path.
//! No rendering types are referenced; the TUI adapter lives in
//! `hjkl-info-popup-tui`.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_info_popup::{InfoPopup, InfoPosition, ContentKind};
//!
//! let popup = InfoPopup::new("registers", "\"a  hello\n\"b  world");
//! assert_eq!(popup.title, " registers ");
//! assert!(!popup.dismissed);
//! assert_eq!(popup.lines().count(), 2);
//! ```

// ── Public types ──────────────────────────────────────────────────────────────

/// How the popup content should be interpreted by the renderer.
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ContentKind {
    /// Plain text — rendered as-is with no markdown interpretation.
    #[default]
    Plain,
    /// CommonMark markdown — the TUI adapter uses `hjkl-markdown-tui` to
    /// parse and render the content with syntax highlighting.
    Markdown,
}

/// How the popup is positioned within the available area.
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum InfoPosition {
    /// Centered horizontally and vertically.  80% wide, 60% tall.
    #[default]
    Centered,
}

/// All state needed to display a centered info popup overlay.
///
/// The popup shows multi-line content from `:reg`, `:marks`, `:jumps`,
/// `:changes`, or the K-key LSP hover path.  Any keypress dismisses it (the
/// event loop sets `dismissed = true` or drops the value).
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InfoPopup {
    /// Popup title, including surrounding spaces (e.g. `" info "`).
    pub title: String,
    /// Full multi-line content string.  Lines are separated by `'\n'`.
    pub content: String,
    /// How `content` should be interpreted by the renderer.
    pub kind: ContentKind,
    /// Positioning strategy.
    pub position: InfoPosition,
    /// Whether the popup has been explicitly dismissed.
    pub dismissed: bool,
}

impl InfoPopup {
    /// Create a plain-text popup with the given `title` and `content`.
    ///
    /// The title is wrapped with a leading and trailing space so ratatui renders
    /// it with padding (e.g. `" registers "`).
    ///
    /// ```rust
    /// use hjkl_info_popup::InfoPopup;
    ///
    /// let p = InfoPopup::new("marks", "  '  1  some/file.rs\n  \"  2  other.rs");
    /// assert_eq!(p.title, " marks ");
    /// assert!(!p.dismissed);
    /// ```
    pub fn new(title: &str, content: impl Into<String>) -> Self {
        Self {
            title: format!(" {title} "),
            content: content.into(),
            kind: ContentKind::Plain,
            position: InfoPosition::Centered,
            dismissed: false,
        }
    }

    /// Create a markdown popup with the given `title` and `content`.
    ///
    /// Used by the K-key LSP hover path; the TUI adapter parses the content
    /// with `hjkl-markdown-tui` for syntax-aware rendering.
    ///
    /// ```rust
    /// use hjkl_info_popup::{InfoPopup, ContentKind};
    ///
    /// let p = InfoPopup::markdown("hover", "# Fn\n\nDoes a thing.");
    /// assert_eq!(p.kind, ContentKind::Markdown);
    /// ```
    pub fn markdown(title: &str, content: impl Into<String>) -> Self {
        Self {
            title: format!(" {title} "),
            content: content.into(),
            kind: ContentKind::Markdown,
            position: InfoPosition::Centered,
            dismissed: false,
        }
    }

    /// Iterator over the content lines.
    ///
    /// ```rust
    /// use hjkl_info_popup::InfoPopup;
    ///
    /// let p = InfoPopup::new("info", "line1\nline2\nline3");
    /// assert_eq!(p.lines().count(), 3);
    /// ```
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.content.lines()
    }

    /// Number of content lines.
    pub fn line_count(&self) -> usize {
        self.content.lines().count().max(1)
    }
}

impl Default for InfoPopup {
    fn default() -> Self {
        Self::new("info", String::new())
    }
}

impl From<String> for InfoPopup {
    /// Convert a raw plain-text string into an `InfoPopup` with the default
    /// `" info "` title.  Convenient for call sites that used to hold
    /// `Option<String>`.
    fn from(content: String) -> Self {
        Self::new("info", content)
    }
}

// ── Geometry helpers ──────────────────────────────────────────────────────────

/// Viewport dimensions for popup placement.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InfoViewport {
    /// Total width of the area the popup may occupy.
    pub width: u16,
    /// Total height of the area the popup may occupy.
    pub height: u16,
}

impl InfoViewport {
    /// Convenience constructor.
    pub fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// Bounding rect returned by [`geometry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct InfoRect {
    /// Left column (0-based).
    pub x: u16,
    /// Top row (0-based).
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

/// Compute the bounding rect for an [`InfoPopup`] within `viewport`.
///
/// For [`InfoPosition::Centered`], the popup occupies 80% of the viewport width
/// and 60% of the height, clamped to at least 4×3.
///
/// ```rust
/// use hjkl_info_popup::{InfoPopup, InfoViewport, geometry};
///
/// let popup = InfoPopup::new("reg", "\"a  hello");
/// let r = geometry(&popup, InfoViewport::new(80, 24));
/// assert!(r.x + r.width <= 80);
/// assert!(r.y + r.height <= 24);
/// ```
pub fn geometry(popup: &InfoPopup, viewport: InfoViewport) -> InfoRect {
    match popup.position {
        InfoPosition::Centered => centered_rect(80, 60, viewport),
        // Future positions handled here.
    }
}

fn centered_rect(pct_x: u16, pct_y: u16, vp: InfoViewport) -> InfoRect {
    let width = (vp.width.saturating_mul(pct_x) / 100).max(4).min(vp.width);
    let height = (vp.height.saturating_mul(pct_y) / 100)
        .max(3)
        .min(vp.height);
    let x = (vp.width.saturating_sub(width)) / 2;
    let y = (vp.height.saturating_sub(height)) / 2;
    InfoRect {
        x,
        y,
        width,
        height,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_wraps_title_with_spaces() {
        let p = InfoPopup::new("registers", "content");
        assert_eq!(p.title, " registers ");
    }

    #[test]
    fn dismissed_defaults_to_false() {
        let p = InfoPopup::new("marks", "");
        assert!(!p.dismissed);
    }

    #[test]
    fn lines_count() {
        let p = InfoPopup::new("jumps", "a\nb\nc");
        assert_eq!(p.lines().count(), 3);
    }

    #[test]
    fn line_count_minimum_one_for_empty() {
        let p = InfoPopup::new("changes", "");
        assert_eq!(p.line_count(), 1);
    }

    #[test]
    fn default_is_info_title_empty_content() {
        let p = InfoPopup::default();
        assert_eq!(p.title, " info ");
        assert!(p.content.is_empty());
    }

    #[test]
    fn new_defaults_to_plain_content_kind() {
        let p = InfoPopup::new("reg", "\"a  hello");
        assert_eq!(p.kind, ContentKind::Plain);
    }

    #[test]
    fn markdown_constructor_sets_markdown_kind() {
        let p = InfoPopup::markdown("hover", "# Title\n\nhello");
        assert_eq!(p.kind, ContentKind::Markdown);
    }

    #[test]
    fn from_string_gives_plain_popup() {
        let p = InfoPopup::from("some text".to_string());
        assert_eq!(p.kind, ContentKind::Plain);
        assert_eq!(p.content, "some text");
    }

    #[test]
    fn geometry_centered_stays_inside_viewport() {
        let p = InfoPopup::new("reg", "hello");
        let r = geometry(&p, InfoViewport::new(80, 24));
        assert!(r.x + r.width <= 80, "overflow right");
        assert!(r.y + r.height <= 24, "overflow bottom");
    }

    #[test]
    fn geometry_centered_80_60_pct() {
        let p = InfoPopup::new("reg", "hello");
        let vp = InfoViewport::new(100, 40);
        let r = geometry(&p, vp);
        // 80% of 100 = 80 width, 60% of 40 = 24 height
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 24);
        // centered: x = (100-80)/2 = 10
        assert_eq!(r.x, 10);
        assert_eq!(r.y, 8); // (40-24)/2
    }

    #[test]
    fn geometry_clamps_to_minimum() {
        let p = InfoPopup::new("reg", "x");
        let r = geometry(&p, InfoViewport::new(3, 2));
        assert!(r.width >= 4 || r.width == 3); // clamped to viewport
        assert!(r.height >= 3 || r.height == 2);
    }
}
