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

/// One-file-per-theme registry. Each entry is `(name, toml)` where the TOML is
/// a SINGLE document carrying BOTH the fixed `[chrome]`/`[mode]`/… UI tables
/// AND the hjkl-theme `[palette]` + `@capture` syntax tables. `AppTheme::from_toml`
/// parses it twice — once as [`UiTheme`], once as [`DotFallbackTheme`] — and
/// each parser ignores the other's tables (neither uses `deny_unknown_fields`).
///
/// `"dark"` / `"light"` are NOT listed here: they keep their historical
/// two-file construction (`default_dark`) so the theme.rs unit tests that assert
/// dark hex values stay green. New bundled themes are single-file and land here.
const BUNDLED: &[(&str, &str)] = &[
    ("tokyonight", include_str!("../themes/tokyonight.toml")),
    ("catppuccin", include_str!("../themes/catppuccin.toml")),
    ("gruvbox", include_str!("../themes/gruvbox.toml")),
    ("nord", include_str!("../themes/nord.toml")),
    ("dracula", include_str!("../themes/dracula.toml")),
    ("onedark", include_str!("../themes/onedark.toml")),
];

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

    /// Light colorscheme. Historically light is *syntax-only*: the chrome stays
    /// the dark UI palette while the syntax layer flips to `DotFallbackTheme::light`.
    /// Preserved verbatim so `:colorscheme light` / `:set background=light` don't
    /// regress.
    pub fn default_light() -> Self {
        let ui = UiTheme::from_toml(UI_DARK_TOML).expect("bundled ui-dark.toml is malformed");
        let syntax = Arc::new(DotFallbackTheme::light());
        Self { ui, syntax }
    }

    /// Parse a single-document theme TOML into a full [`AppTheme`]: the same
    /// string feeds both [`UiTheme::from_toml`] (reads the `[chrome]`/`[mode]`/…
    /// tables) and [`DotFallbackTheme::from_toml`] (reads `[palette]` + `@capture`
    /// keys). The two parsers ignore each other's tables — neither derives
    /// `deny_unknown_fields` — so one file drives the whole theme.
    pub fn from_toml(s: &str) -> Result<Self> {
        let ui = UiTheme::from_toml(s)?;
        let syntax = Arc::new(DotFallbackTheme::from_toml(s)?);
        Ok(Self { ui, syntax })
    }
}

/// Resolve a colorscheme name to a fully-built [`AppTheme`]. Returns `None` for
/// unknown names so callers can emit `E185` / a startup warning. `"dark"` and
/// `"light"` map to the historical two-file builders; every other name is looked
/// up in [`BUNDLED`] and parsed via [`AppTheme::from_toml`].
pub fn load_named(name: &str) -> Option<AppTheme> {
    match name {
        "dark" => Some(AppTheme::default_dark()),
        "light" => Some(AppTheme::default_light()),
        _ => BUNDLED
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, toml)| AppTheme::from_toml(toml).ok()),
    }
}

/// All colorscheme names hjkl knows how to load: the two historical schemes
/// (`dark`, `light`) plus every single-file [`BUNDLED`] theme. Used to validate
/// `:colorscheme <name>` and to power future completion.
pub fn bundled_theme_names() -> Vec<&'static str> {
    let mut names = vec!["dark", "light"];
    names.extend(BUNDLED.iter().map(|(n, _)| *n));
    names
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
    /// Editor text-area + gutter background. Painted as the base layer under
    /// syntax/cursorline (see `BufferView::background`) so the terminal's own
    /// background doesn't show through. Honoured only when `theme.transparent`
    /// is `false`.
    pub background: Color,
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

    // [hop]
    /// Foreground color for hop/easymotion labels painted over targets (#197).
    /// High-contrast against the label background.
    pub hop_label_fg: Color,
    /// Background color for hop/easymotion labels (#197). Bright accent so
    /// labels pop against any buffer content.
    pub hop_label_bg: Color,
}

impl UiTheme {
    /// Parse a TOML string into a fully-populated `UiTheme`. Errors if any
    /// declared section/field is missing or any color string is not a
    /// 7-character `#rrggbb` literal.
    pub fn from_toml(s: &str) -> Result<Self> {
        let raw: RawUiTheme = toml::from_str(s).context("parse ui theme TOML")?;
        Ok(Self {
            background: parse_hex(&raw.chrome.background)?,
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
            hop_label_fg: parse_hex(&raw.hop.label_fg)?,
            hop_label_bg: parse_hex(&raw.hop.label_bg)?,
        })
    }

    /// THE style painted over search-pattern matches — hlsearch in a normal
    /// buffer (`render::buffer_pane`'s `search_bg`), the substitute-confirm
    /// current-match flash, and the quickfix dock's match overlay all read
    /// this one accessor so a "search match" always looks the same
    /// everywhere. Both bg AND fg are set, which is what lets the style WIN
    /// over syntax highlighting when patched/layered on top of it.
    pub fn search_match_style(&self) -> ratatui::style::Style {
        ratatui::style::Style::default()
            .bg(self.search_bg)
            .fg(self.search_fg)
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
    hop: RawHop,
}

#[derive(Deserialize)]
struct RawChrome {
    background: String,
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

#[derive(Deserialize)]
struct RawHop {
    label_fg: String,
    label_bg: String,
}

fn parse_hex(s: &str) -> Result<Color> {
    let bytes = s.as_bytes();
    // `!s.is_ascii()` guard: the length check is in BYTES, so a multibyte
    // string like "#€ab" (7 bytes) would otherwise panic on the mid-char
    // slices below instead of returning an error.
    if bytes.len() != 7 || bytes[0] != b'#' || !s.is_ascii() {
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
    fn parse_hex_rejects_multibyte_without_panicking() {
        // 7 BYTES but not 7 ASCII chars — '€' is 3 bytes, so slicing byte
        // ranges would split the char and panic without the is_ascii guard.
        assert!(parse_hex("#\u{20ac}abc").is_err());
        assert!(parse_hex("#\u{e9}\u{e9}\u{e9}").is_err());
        // Sanity: valid input still parses.
        assert_eq!(parse_hex("#0a0B0c").unwrap(), Color::Rgb(0x0a, 0x0b, 0x0c));
    }

    #[test]
    fn bundled_dark_theme_loads() {
        let theme = AppTheme::default_dark();
        // Spot-check a few high-traffic fields.
        assert_eq!(theme.ui.text, Color::Rgb(0xe5, 0xe9, 0xf0));
        assert_eq!(theme.ui.mode_insert_bg, Color::Rgb(0x7e, 0xe7, 0x87));
        assert_eq!(theme.ui.cursor_line_bg, Color::Rgb(0x2a, 0x32, 0x40));
        assert_eq!(
            theme.ui.fold_line_bg,
            Color::Rgb(0x3a, 0x2f, 0x50),
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

    /// #303: the single-file tokyonight theme must load BOTH halves — all 34
    /// UI-chrome fields (via `UiTheme::from_toml`) and a valid syntax layer
    /// (via `DotFallbackTheme::from_toml`) — from one document. Neither parser
    /// may choke on the other's tables.
    #[test]
    fn tokyonight_single_file_loads_both_halves() {
        let toml = include_str!("../themes/tokyonight.toml");
        let theme =
            AppTheme::from_toml(toml).expect("tokyonight.toml must parse as a full AppTheme");
        // UI half: the editor background is the Night `bg`.
        assert_eq!(theme.ui.background, Color::Rgb(0x1a, 0x1b, 0x26));
        // A few more UI fields to prove the whole 34-field struct populated.
        assert_eq!(theme.ui.mode_insert_bg, Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(theme.ui.cursor_line_bg, Color::Rgb(0x29, 0x2e, 0x42));
        assert_eq!(theme.ui.hop_label_bg, Color::Rgb(0xff, 0x9e, 0x64));
        // Syntax half: high-traffic captures resolve.
        assert!(theme.syntax.style("@keyword").is_some());
        assert!(theme.syntax.style("@function").is_some());
        assert!(theme.syntax.style("@string").is_some());
        assert!(theme.syntax.style("@comment").is_some());
    }

    /// #303 Slice B: the five additional bundled single-file themes must each
    /// load BOTH halves — all 34 UI-chrome fields (`UiTheme::from_toml` hard-fails
    /// on any missing key, so a successful parse proves the full set is present)
    /// AND a non-empty syntax capture set (`DotFallbackTheme::from_toml`) — from
    /// one document, keyed by their expected editor `background`.
    #[test]
    fn slice_b_bundled_themes_load_both_halves() {
        // (name, editor background from the theme's [chrome].background).
        let cases: &[(&str, Color)] = &[
            ("catppuccin", Color::Rgb(0x1e, 0x1e, 0x2e)),
            ("gruvbox", Color::Rgb(0x28, 0x28, 0x28)),
            ("nord", Color::Rgb(0x2e, 0x34, 0x40)),
            ("dracula", Color::Rgb(0x28, 0x2a, 0x36)),
            ("onedark", Color::Rgb(0x28, 0x2c, 0x34)),
        ];
        for (name, bg) in cases {
            let toml = BUNDLED
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} must be registered in BUNDLED"))
                .1;
            let theme = AppTheme::from_toml(toml)
                .unwrap_or_else(|e| panic!("{name}.toml must parse as a full AppTheme: {e}"));

            // UI half: the editor background matches the theme's [chrome] bg.
            // A successful parse already guarantees all 34 UiTheme fields are
            // present — the loader rejects any missing key.
            assert_eq!(
                theme.ui.background, *bg,
                "{name} editor background must match its [chrome].background"
            );

            // Syntax half: the high-traffic captures resolve to a non-empty spec.
            for cap in [
                "@keyword",
                "@function",
                "@string",
                "@comment",
                "@type",
                "@number",
            ] {
                assert!(
                    theme.syntax.style(cap).is_some(),
                    "{name} must resolve syntax capture {cap}"
                );
            }

            // load_named must resolve to the same background.
            let via_named =
                load_named(name).unwrap_or_else(|| panic!("load_named({name:?}) must be Some"));
            assert_eq!(via_named.ui.background, *bg);
        }
    }

    #[test]
    fn load_named_resolves_bundled_and_rejects_unknown() {
        assert!(load_named("tokyonight").is_some());
        assert!(load_named("catppuccin").is_some());
        assert!(load_named("gruvbox").is_some());
        assert!(load_named("nord").is_some());
        assert!(load_named("dracula").is_some());
        assert!(load_named("onedark").is_some());
        assert!(load_named("dark").is_some());
        assert!(load_named("light").is_some());
        assert!(load_named("nonesuch").is_none());
        // tokyonight via load_named matches from_toml directly.
        let via_named = load_named("tokyonight").unwrap();
        assert_eq!(via_named.ui.background, Color::Rgb(0x1a, 0x1b, 0x26));
    }

    #[test]
    fn bundled_theme_names_contains_all_schemes() {
        let names = bundled_theme_names();
        assert!(names.contains(&"tokyonight"));
        assert!(names.contains(&"catppuccin"));
        assert!(names.contains(&"gruvbox"));
        assert!(names.contains(&"nord"));
        assert!(names.contains(&"dracula"));
        assert!(names.contains(&"onedark"));
        assert!(names.contains(&"dark"));
        assert!(names.contains(&"light"));
    }

    /// `default_light` preserves the historical syntax-only light behaviour: the
    /// chrome stays the dark UI palette while the syntax layer flips to light.
    #[test]
    fn default_light_keeps_dark_chrome_with_light_syntax() {
        let light = AppTheme::default_light();
        let dark = AppTheme::default_dark();
        assert_eq!(
            light.ui.background, dark.ui.background,
            "light is syntax-only: chrome must stay the dark UI palette"
        );
        assert!(light.syntax.style("@keyword").is_some());
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
