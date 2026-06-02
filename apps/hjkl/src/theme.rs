//! App-wide theme: UI chrome colors + the syntax-highlighting theme that
//! overrides `hjkl-bonsai`'s bundled default.
//!
//! Both halves are TOML files baked into the binary via `include_str!`.
//! `hjkl-bonsai` keeps its own `DotFallbackTheme::dark()` /
//! `light()` for other consumers; we layer our website-palette override
//! on top by parsing `themes/syntax-dark.toml` through the public
//! `DotFallbackTheme::from_toml()` entrypoint.
//!
//! # Schema
//!
//! `themes/ui-dark.toml` is a fixed-shape TOML with five tables —
//! `[chrome] [mode] [cursor_line] [search] [picker] [form] [status]
//! [recording]`. Every key in [`UiTheme`] must be present; the loader
//! rejects missing fields rather than silently defaulting so a typo in
//! the TOML fails at startup, not at the first render that uses the
//! field.
//!
//! `themes/syntax-dark.toml` follows `DotFallbackTheme`'s existing flat
//! `<capture> = { fg, bg, bold, italic, underline }` layout — see
//! `hjkl-bonsai/themes/default-dark.toml` for the canonical shape.

use std::sync::Arc;

use anyhow::{Context, Result};
use hjkl_bonsai::DotFallbackTheme;
use ratatui::style::Color;
use serde::Deserialize;

const UI_DARK_TOML: &str = include_str!("../themes/ui-dark.toml");
const SYNTAX_DARK_TOML: &str = include_str!("../themes/syntax-dark.toml");

/// Bundled bright-on-dark theme. UI chrome + syntax highlighting in one
/// struct. Constructed once at startup and threaded through render
/// helpers and picker sources.
#[derive(Clone)]
pub struct AppTheme {
    pub ui: UiTheme,
    pub syntax: Arc<DotFallbackTheme>,
}

impl AppTheme {
    /// Default dark theme — palette mirrors hjkl.kryptic.sh.
    pub fn default_dark() -> Self {
        let ui = UiTheme::from_toml(UI_DARK_TOML).expect("bundled ui-dark.toml is malformed");
        let syntax = Arc::new(
            DotFallbackTheme::from_toml(SYNTAX_DARK_TOML)
                .expect("bundled syntax-dark.toml is malformed"),
        );
        Self { ui, syntax }
    }
}

/// UI chrome palette — status bar, mode badges, cursor row, picker
/// selection, search highlight, form mode bgs, recording badge.
///
/// Each field maps to one [`Color`] used at exactly one render site. The
/// names mirror the table.key path in `themes/ui-dark.toml` so a grep
/// from either side finds the other.
#[derive(Clone)]
pub struct UiTheme {
    // [chrome]
    pub text: Color,
    pub text_dim: Color,
    pub panel_bg: Color,
    pub surface_bg: Color,
    pub border: Color,
    pub border_active: Color,
    pub gutter: Color,
    pub on_accent: Color,
    /// Color for vim's NonText highlight group — the `~` tilde markers
    /// on screen rows past the last buffer line. Defaults to the gutter
    /// color (dim fg), matching most vim themes.
    pub non_text: Color,

    // [mode]
    pub mode_normal_bg: Color,
    pub mode_insert_bg: Color,
    pub mode_visual_bg: Color,

    // [cursor_line]
    pub cursor_line_bg: Color,
    /// Fainter cursor-line bg painted on UNFOCUSED windows so the current line
    /// stays visible (e.g. the explorer's selection) without the cursor.
    pub cursor_line_inactive_bg: Color,
    pub cursor_column_bg: Color,
    pub fold_line_bg: Color,

    // [colorcolumn]
    pub colorcolumn_bg: Color,

    // [search]
    pub search_bg: Color,
    pub search_fg: Color,

    // [picker]
    pub picker_selection_bg: Color,

    // [form]
    pub form_normal_bg: Color,
    pub form_insert_bg: Color,
    pub form_tag_normal_fg: Color,
    pub form_tag_insert_fg: Color,

    // [status]
    pub status_dirty_marker: Color,

    // [recording]
    pub recording_bg: Color,
    pub recording_fg: Color,

    // [indent_flash]
    /// Background painted over rows touched by the `=` auto-indent operator
    /// for [`crate::app::INDENT_FLASH_DURATION`] (150 ms). Instant on, instant
    /// off — no fade. Defaults to a muted amber tint so it's distinct from the
    /// search highlight without being jarring.
    pub indent_flash_bg: Color,

    // [indent_guide]
    /// Fg color for inactive indent guide lines. Muted so guides don't compete
    /// with code. Default: very dark gray (#3c3c3c).
    pub indent_guide_fg: Color,
    /// Fg color for the active (cursor's indent level) guide line.
    /// Slightly brighter than inactive. Default: medium gray (#787878).
    pub indent_guide_active_fg: Color,

    // [match_paren]
    /// Background painted over both matched bracket cells when matchparen is
    /// active. Chosen to be distinct from cursor_line and search without being
    /// jarring — muted slate fits the ui-dark palette.
    pub match_paren_bg: Color,
}

impl UiTheme {
    /// Parse a TOML string into a fully-populated `UiTheme`. Errors if any
    /// declared section/field is missing or any color string is not a
    /// 7-character `#rrggbb` literal.
    pub fn from_toml(s: &str) -> Result<Self> {
        let raw: RawUiTheme = toml::from_str(s).context("parse ui theme TOML")?;
        Ok(Self {
            text: parse_hex(&raw.chrome.text)?,
            text_dim: parse_hex(&raw.chrome.text_dim)?,
            panel_bg: parse_hex(&raw.chrome.panel_bg)?,
            surface_bg: parse_hex(&raw.chrome.surface_bg)?,
            border: parse_hex(&raw.chrome.border)?,
            border_active: parse_hex(&raw.chrome.border_active)?,
            gutter: parse_hex(&raw.chrome.gutter)?,
            on_accent: parse_hex(&raw.chrome.on_accent)?,
            non_text: parse_hex(&raw.chrome.non_text)?,
            mode_normal_bg: parse_hex(&raw.mode.normal_bg)?,
            mode_insert_bg: parse_hex(&raw.mode.insert_bg)?,
            mode_visual_bg: parse_hex(&raw.mode.visual_bg)?,
            cursor_line_bg: parse_hex(&raw.cursor_line.bg)?,
            cursor_line_inactive_bg: parse_hex(&raw.cursor_line.inactive_bg)?,
            cursor_column_bg: parse_hex(&raw.cursor_line.column_bg)?,
            fold_line_bg: parse_hex(&raw.cursor_line.fold_bg)?,
            colorcolumn_bg: parse_hex(&raw.colorcolumn.bg)?,
            search_bg: parse_hex(&raw.search.bg)?,
            search_fg: parse_hex(&raw.search.fg)?,
            picker_selection_bg: parse_hex(&raw.picker.selection_bg)?,
            form_normal_bg: parse_hex(&raw.form.normal_bg)?,
            form_insert_bg: parse_hex(&raw.form.insert_bg)?,
            form_tag_normal_fg: parse_hex(&raw.form.tag_normal_fg)?,
            form_tag_insert_fg: parse_hex(&raw.form.tag_insert_fg)?,
            status_dirty_marker: parse_hex(&raw.status.dirty_marker)?,
            recording_bg: parse_hex(&raw.recording.bg)?,
            recording_fg: parse_hex(&raw.recording.fg)?,
            indent_flash_bg: parse_hex(&raw.indent_flash.bg)?,
            indent_guide_fg: parse_hex(&raw.indent_guide.fg)?,
            indent_guide_active_fg: parse_hex(&raw.indent_guide.active_fg)?,
            match_paren_bg: parse_hex(&raw.match_paren.bg)?,
        })
    }
}

#[derive(Deserialize)]
struct RawUiTheme {
    chrome: RawChrome,
    mode: RawMode,
    cursor_line: RawCursorLine,
    colorcolumn: RawColorColumn,
    search: RawSearch,
    picker: RawPicker,
    form: RawForm,
    status: RawStatus,
    recording: RawRecording,
    indent_flash: RawIndentFlash,
    indent_guide: RawIndentGuide,
    match_paren: RawMatchParen,
}

#[derive(Deserialize)]
struct RawChrome {
    text: String,
    text_dim: String,
    panel_bg: String,
    surface_bg: String,
    border: String,
    border_active: String,
    gutter: String,
    on_accent: String,
    non_text: String,
}

#[derive(Deserialize)]
struct RawMode {
    normal_bg: String,
    insert_bg: String,
    visual_bg: String,
}

#[derive(Deserialize)]
struct RawCursorLine {
    bg: String,
    inactive_bg: String,
    column_bg: String,
    fold_bg: String,
}

#[derive(Deserialize)]
struct RawColorColumn {
    bg: String,
}

#[derive(Deserialize)]
struct RawSearch {
    bg: String,
    fg: String,
}

#[derive(Deserialize)]
struct RawPicker {
    selection_bg: String,
}

#[derive(Deserialize)]
struct RawForm {
    normal_bg: String,
    insert_bg: String,
    tag_normal_fg: String,
    tag_insert_fg: String,
}

#[derive(Deserialize)]
struct RawStatus {
    dirty_marker: String,
}

#[derive(Deserialize)]
struct RawRecording {
    bg: String,
    fg: String,
}

#[derive(Deserialize)]
struct RawIndentFlash {
    bg: String,
}

#[derive(Deserialize)]
struct RawIndentGuide {
    fg: String,
    active_fg: String,
}

#[derive(Deserialize)]
struct RawMatchParen {
    bg: String,
}

fn parse_hex(s: &str) -> Result<Color> {
    let bytes = s.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        anyhow::bail!("expected #rrggbb hex color, got {s:?}");
    }
    let r = u8::from_str_radix(&s[1..3], 16).with_context(|| format!("bad red byte in {s:?}"))?;
    let g = u8::from_str_radix(&s[3..5], 16).with_context(|| format!("bad green byte in {s:?}"))?;
    let b = u8::from_str_radix(&s[5..7], 16).with_context(|| format!("bad blue byte in {s:?}"))?;
    Ok(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_bonsai::Theme;

    #[test]
    fn bundled_dark_theme_loads() {
        let theme = AppTheme::default_dark();
        // Spot-check a few high-traffic fields.
        assert_eq!(theme.ui.text, Color::Rgb(0xe5, 0xe9, 0xf0));
        assert_eq!(theme.ui.mode_insert_bg, Color::Rgb(0x7e, 0xe7, 0x87));
        assert_eq!(theme.ui.cursor_line_bg, Color::Rgb(0x2a, 0x32, 0x40));
        assert_eq!(
            theme.ui.fold_line_bg,
            Color::Rgb(0x3a, 0x4a, 0x5a),
            "fold_line_bg must match ui-dark.toml fold_bg value"
        );
        // indent_flash_bg must parse — muted amber tint.
        assert_eq!(
            theme.ui.indent_flash_bg,
            Color::Rgb(0x2e, 0x28, 0x10),
            "indent_flash_bg must match ui-dark.toml value"
        );
        // match_paren_bg must parse — muted slate.
        assert_eq!(
            theme.ui.match_paren_bg,
            Color::Rgb(0x45, 0x47, 0x5a),
            "match_paren_bg must match ui-dark.toml value"
        );
        // Syntax theme must resolve a known capture.
        assert!(theme.syntax.style("@keyword").is_some());
    }

    /// Regression: tree-sitter-markdown emits `@markup.*` capture names
    /// (`@markup.heading.1`, `@markup.italic`, `@markup.raw`, …) which the
    /// theme used to omit, so every markdown markup span rendered unstyled.
    /// The capture-fallback chain right-truncates by dot, so each leaf is
    /// expected to resolve to a non-empty StyleSpec — directly or via
    /// fallback to its `@markup.<root>` ancestor.
    #[test]
    fn syntax_theme_covers_markdown_markup_captures() {
        let theme = AppTheme::default_dark();
        let must_resolve = [
            "@markup.heading",
            "@markup.heading.1",
            "@markup.heading.2",
            "@markup.heading.3",
            "@markup.heading.4",
            "@markup.heading.5",
            "@markup.heading.6",
            "@markup.italic",
            "@markup.strong",
            "@markup.strikethrough",
            "@markup.raw",
            "@markup.raw.block",
            "@markup.link",
            "@markup.link.label",
            "@markup.link.url",
            "@markup.list",
            "@markup.list.checked",
            "@markup.list.unchecked",
            "@markup.quote",
            "@label",
            // Already-supported captures used by markdown.scm — pinned so a
            // future theme-prune doesn't quietly regress them.
            "@punctuation.special",
            "@punctuation.delimiter",
            "@string.escape",
            "@keyword.directive",
        ];
        for cap in must_resolve {
            assert!(
                theme.syntax.style(cap).is_some(),
                "syntax-dark.toml must resolve {cap} for tree-sitter-markdown highlighting"
            );
        }
    }

    /// Regression: `@markup.heading.1` must inherit bold from
    /// `@markup.heading` (or override it) — markdown headings rendering as
    /// plain text on terminals was the 0.18.0 user-visible bug that
    /// motivated this test.
    #[test]
    fn markup_heading_renders_bold() {
        let theme = AppTheme::default_dark();
        let spec = theme
            .syntax
            .style("@markup.heading.1")
            .expect("must resolve");
        assert!(
            spec.modifiers.bold,
            "@markup.heading.1 must be bold so ## headers stand out"
        );
        let spec_h2 = theme
            .syntax
            .style("@markup.heading.2")
            .expect("must resolve");
        assert!(spec_h2.modifiers.bold, "@markup.heading.2 must be bold");
    }

    /// Regression: `@markup.strikethrough` must actually toggle the
    /// strikethrough modifier — hjkl-theme accepts the `"strikethrough"`
    /// token in `modifiers = […]`, an earlier draft of this theme used
    /// `"italic"` as a stand-in.
    /// Regression: `@markup.raw.block` must carry a `bg` so the layered
    /// resolver in `hjkl-buffer 0.6.1+` can tint markdown fenced/indented
    /// code blocks underneath the injected language's `fg`-only spans.
    /// Without a `bg`, the broad raw-block span contributes nothing and
    /// code blocks look identical to surrounding prose.
    #[test]
    fn markup_raw_block_has_bg_for_code_block_tint() {
        let theme = AppTheme::default_dark();
        let spec = theme
            .syntax
            .style("@markup.raw.block")
            .expect("must resolve");
        assert!(
            spec.bg.is_some(),
            "@markup.raw.block must set a bg so layered resolver can tint code blocks"
        );
    }

    #[test]
    fn markup_strikethrough_uses_strikethrough_modifier() {
        let theme = AppTheme::default_dark();
        let spec = theme
            .syntax
            .style("@markup.strikethrough")
            .expect("must resolve");
        assert!(
            spec.modifiers.strikethrough,
            "@markup.strikethrough must actually strike through"
        );
    }

    #[test]
    fn parse_hex_rejects_short_string() {
        assert!(parse_hex("#fff").is_err());
    }

    #[test]
    fn parse_hex_rejects_missing_hash() {
        assert!(parse_hex("ff9e64a").is_err());
    }

    #[test]
    fn parse_hex_rejects_non_hex() {
        assert!(parse_hex("#zzggbb").is_err());
    }

    #[test]
    fn ui_theme_rejects_missing_field() {
        let s = r##"
            [chrome]
            text = "#000000"
            # other chrome fields missing
        "##;
        assert!(UiTheme::from_toml(s).is_err());
    }
}
