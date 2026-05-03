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

    // [mode]
    pub mode_normal_bg: Color,
    pub mode_insert_bg: Color,
    pub mode_visual_bg: Color,

    // [cursor_line]
    pub cursor_line_bg: Color,

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
            mode_normal_bg: parse_hex(&raw.mode.normal_bg)?,
            mode_insert_bg: parse_hex(&raw.mode.insert_bg)?,
            mode_visual_bg: parse_hex(&raw.mode.visual_bg)?,
            cursor_line_bg: parse_hex(&raw.cursor_line.bg)?,
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
        })
    }
}

#[derive(Deserialize)]
struct RawUiTheme {
    chrome: RawChrome,
    mode: RawMode,
    cursor_line: RawCursorLine,
    search: RawSearch,
    picker: RawPicker,
    form: RawForm,
    status: RawStatus,
    recording: RawRecording,
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
        // Syntax theme must resolve a known capture.
        assert!(theme.syntax.style("keyword").is_some());
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
