use anyhow::{Context, Result};

pub use hjkl_theme::StyleSpec;

/// Syntax theme. Implementors map capture names to [`StyleSpec`] values.
/// The dot-fallback resolution (`@function.method.builtin` → `@function.method`
/// → `@function` → `None`) is provided by `DotFallbackTheme`.
pub trait Theme: Send + Sync {
    /// Return the style for a capture name, or `None` to skip styling.
    fn style(&self, capture: &str) -> Option<&hjkl_theme::StyleSpec>;
}

// ---------------------------------------------------------------------------
// DotFallbackTheme
// ---------------------------------------------------------------------------

/// A theme loaded from a TOML string in hjkl-theme schema format.
/// Resolves captures via dot-fallback:
/// `@function.method.builtin` → `@function.method` → `@function` → `None`.
pub struct DotFallbackTheme {
    inner: hjkl_theme::Theme,
}

impl DotFallbackTheme {
    /// Parse from a TOML string following the hjkl-theme schema.
    ///
    /// Capture keys must be `@`-prefixed TS names. Modifiers use the
    /// `modifiers = ["bold", "italic"]` array form.
    pub fn from_toml(toml_str: &str) -> Result<Self> {
        let inner = hjkl_theme::Theme::from_toml_str(toml_str).context("parse theme TOML")?;
        Ok(Self { inner })
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
    fn style(&self, capture: &str) -> Option<&hjkl_theme::StyleSpec> {
        self.inner.captures.resolve(capture)
    }
}

#[cfg(test)]
mod tests {
    use hjkl_theme::Color;

    use super::*;

    #[test]
    fn theme_dot_fallback_exact_match() {
        let theme = DotFallbackTheme::dark();
        let s = theme.style("@keyword");
        assert!(s.is_some(), "expected style for '@keyword'");
        assert!(s.unwrap().modifiers.bold);
    }

    #[test]
    fn theme_dot_fallback_partial_match() {
        let theme = DotFallbackTheme::dark();
        // "@function.method.builtin" not in theme -> falls to "@function.method" -> "@function"
        let s = theme.style("@function.method.builtin");
        assert!(
            s.is_some(),
            "expected fallback style for '@function.method.builtin'"
        );
    }

    #[test]
    fn theme_dot_fallback_unknown_returns_none() {
        let theme = DotFallbackTheme::dark();
        // Completely unknown key with no partial matches returns None (no "default" key in schema).
        let s = theme.style("@zzzunknown.deep.capture");
        assert!(
            s.is_none(),
            "expected None for unknown capture with no fallback"
        );
    }

    #[test]
    fn theme_light_loads() {
        let theme = DotFallbackTheme::light();
        assert!(theme.style("@keyword").is_some());
    }

    #[test]
    fn theme_from_toml_invalid_color_errors() {
        let bad = r##""@keyword" = { fg = "#zzzzzz", modifiers = ["bold"] }"##;
        assert!(DotFallbackTheme::from_toml(bad).is_err());
    }

    #[test]
    fn dark_keyword_fg_matches_palette() {
        let theme = DotFallbackTheme::dark();
        let spec = theme
            .style("@keyword")
            .expect("@keyword must exist in dark theme");
        // mauve = "#cc99cc" in the dark palette
        assert_eq!(spec.fg, Some(Color::rgb(0xcc, 0x99, 0xcc)));
        assert!(spec.modifiers.bold);
    }

    #[test]
    fn dark_default_toml_parses_keyword_captures() {
        let theme = DotFallbackTheme::dark();
        for cap in [
            "@keyword",
            "@string",
            "@comment",
            "@function",
            "@type",
            "@variable",
            "@operator",
        ] {
            assert!(
                theme.style(cap).is_some(),
                "expected capture '{cap}' in default-dark.toml"
            );
        }
    }

    #[test]
    fn light_keyword_fg_matches_palette() {
        let theme = DotFallbackTheme::light();
        let spec = theme
            .style("@keyword")
            .expect("@keyword must exist in light theme");
        // mauve = "#7b368f" in the light palette
        assert_eq!(spec.fg, Some(Color::rgb(0x7b, 0x36, 0x8f)));
        assert!(spec.modifiers.bold);
    }
}
