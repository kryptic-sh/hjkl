use std::collections::HashMap;

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::Deserialize;

/// Visual style for a syntax capture. Mirrors ratatui's `Style` but is
/// self-contained so callers can adapt it to any renderer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Style {
    /// Convert into a ratatui `Style`.
    pub fn to_ratatui(&self) -> ratatui::style::Style {
        let mut s = ratatui::style::Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        let mut mods = ratatui::style::Modifier::empty();
        if self.bold {
            mods |= ratatui::style::Modifier::BOLD;
        }
        if self.italic {
            mods |= ratatui::style::Modifier::ITALIC;
        }
        if self.underline {
            mods |= ratatui::style::Modifier::UNDERLINED;
        }
        s.add_modifier(mods)
    }
}

/// Syntax theme. Implementors map capture names to `Style` values.
/// The dot-fallback resolution (`function.method.builtin` → `function.method`
/// → `function` → default) is provided by `DotFallbackTheme`.
pub trait Theme: Send + Sync {
    /// Return the style for a capture name, or `None` to skip styling.
    fn style(&self, capture: &str) -> Option<Style>;
}

// ---------------------------------------------------------------------------
// TOML deserialization helpers
// ---------------------------------------------------------------------------

fn parse_hex_color(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[derive(Debug, Deserialize, Default)]
struct RawStyle {
    fg: Option<String>,
    bg: Option<String>,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    underline: bool,
}

impl TryFrom<RawStyle> for Style {
    type Error = anyhow::Error;

    fn try_from(raw: RawStyle) -> Result<Self> {
        let fg = raw
            .fg
            .as_deref()
            .map(|s| parse_hex_color(s).with_context(|| format!("invalid fg color: {s}")))
            .transpose()?;
        let bg = raw
            .bg
            .as_deref()
            .map(|s| parse_hex_color(s).with_context(|| format!("invalid bg color: {s}")))
            .transpose()?;
        Ok(Style {
            fg,
            bg,
            bold: raw.bold,
            italic: raw.italic,
            underline: raw.underline,
        })
    }
}

// ---------------------------------------------------------------------------
// DotFallbackTheme
// ---------------------------------------------------------------------------

/// A theme loaded from a TOML map. Resolves captures via dot-fallback:
/// `function.method.builtin` → `function.method` → `function` → `default`.
pub struct DotFallbackTheme {
    styles: HashMap<String, Style>,
    default: Option<Style>,
}

impl DotFallbackTheme {
    /// Parse from a TOML string of the form:
    /// ```toml
    /// "keyword"         = { fg = "#cc99cc", bold = true }
    /// "keyword.control" = { fg = "#ffaaaa", bold = true }
    /// default           = { fg = "#d8dee9" }
    /// ```
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        // The TOML is a flat table with string keys mapping to style tables.
        let raw: HashMap<String, RawStyle> =
            toml::from_str(toml_str).context("failed to parse theme TOML")?;

        let mut styles = HashMap::with_capacity(raw.len());
        let mut default = None;

        for (key, raw_style) in raw {
            let style = Style::try_from(raw_style).with_context(|| format!("key {key:?}"))?;
            if key == "default" {
                default = Some(style);
            } else {
                styles.insert(key, style);
            }
        }

        Ok(Self { styles, default })
    }

    /// Built-in dark theme (embedded at compile time).
    pub fn dark() -> Self {
        Self::from_toml(include_str!("../themes/default-dark.toml"))
            .expect("bundled default-dark.toml is always valid")
    }

    /// Built-in light theme (embedded at compile time).
    pub fn light() -> Self {
        Self::from_toml(include_str!("../themes/default-light.toml"))
            .expect("bundled default-light.toml is always valid")
    }
}

impl Theme for DotFallbackTheme {
    /// Resolve `capture` via dot-fallback:
    /// `function.method.builtin` → `function.method` → `function` → `default`.
    fn style(&self, capture: &str) -> Option<Style> {
        let mut key = capture;
        loop {
            if let Some(s) = self.styles.get(key) {
                return Some(s.clone());
            }
            // Strip the last `.segment` and retry.
            if let Some(pos) = key.rfind('.') {
                key = &key[..pos];
            } else {
                // No more segments — fall through to `default`.
                break;
            }
        }
        self.default.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_dot_fallback_exact_match() {
        let theme = DotFallbackTheme::dark();
        // "keyword" exists directly
        let s = theme.style("keyword");
        assert!(s.is_some(), "expected style for 'keyword'");
        assert!(s.unwrap().bold);
    }

    #[test]
    fn theme_dot_fallback_partial_match() {
        let theme = DotFallbackTheme::dark();
        // "function.method.builtin" not in theme -> falls to "function"
        let s = theme.style("function.method.builtin");
        assert!(
            s.is_some(),
            "expected fallback style for 'function.method.builtin'"
        );
    }

    #[test]
    fn theme_dot_fallback_unknown_returns_default() {
        let theme = DotFallbackTheme::dark();
        // Completely unknown key falls back to `default`
        let s = theme.style("zzzunknown.deep.capture");
        assert!(s.is_some(), "expected default style for unknown capture");
    }

    #[test]
    fn theme_light_loads() {
        let theme = DotFallbackTheme::light();
        assert!(theme.style("keyword").is_some());
    }

    #[test]
    fn theme_from_toml_invalid_color_errors() {
        // Use ##- raw string delimiter to allow # inside the literal.
        let bad = r##""keyword" = { fg = "#zzzzzz" }"##;
        assert!(DotFallbackTheme::from_toml(bad).is_err());
    }

    #[test]
    fn style_to_ratatui_roundtrip() {
        let style = Style {
            fg: Some(Color::Rgb(100, 150, 200)),
            bold: true,
            italic: true,
            ..Default::default()
        };
        let r = style.to_ratatui();
        assert!(r.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(r.add_modifier.contains(ratatui::style::Modifier::ITALIC));
    }
}
