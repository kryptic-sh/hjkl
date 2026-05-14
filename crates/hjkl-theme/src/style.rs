use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

use crate::{
    ThemeError,
    color::{Color, ColorRef},
};

// ---------------------------------------------------------------------------
// UI surface styles
// ---------------------------------------------------------------------------

/// Resolved UI surface styles.
#[derive(Clone, Default, Debug)]
pub struct UiStyles {
    pub background: Option<Color>,
    pub foreground: Option<Color>,
    pub cursor: Option<Color>,
    pub cursorline: Option<Color>,
    pub statusline: Option<StyleSpec>,
    pub statusline_inactive: Option<StyleSpec>,
    pub gutter: Option<Color>,
    pub gutter_current: Option<Color>,
    pub popup: Option<StyleSpec>,
    pub selection: Option<StyleSpec>,
    pub diagnostic_error: Option<Color>,
    pub diagnostic_warn: Option<Color>,
}

/// Raw UI section from TOML before palette resolution.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawUiStyles {
    pub background: Option<ColorRef>,
    pub foreground: Option<ColorRef>,
    pub cursor: Option<ColorRef>,
    pub cursorline: Option<ColorRef>,
    pub statusline: Option<RawStyleSpec>,
    #[serde(rename = "statusline.inactive")]
    pub statusline_inactive: Option<RawStyleSpec>,
    pub gutter: Option<ColorRef>,
    #[serde(rename = "gutter.current")]
    pub gutter_current: Option<ColorRef>,
    pub popup: Option<RawStyleSpec>,
    pub selection: Option<RawStyleSpec>,
    #[serde(rename = "diagnostic.error")]
    pub diagnostic_error: Option<ColorRef>,
    #[serde(rename = "diagnostic.warn")]
    pub diagnostic_warn: Option<ColorRef>,
}

impl RawUiStyles {
    pub(crate) fn resolve(self, palette: &HashMap<String, Color>) -> Result<UiStyles, ThemeError> {
        let resolve_color = |c: Option<ColorRef>| c.map(|cr| cr.resolve(palette)).transpose();
        let resolve_style = |s: Option<RawStyleSpec>| s.map(|rs| rs.resolve(palette)).transpose();
        Ok(UiStyles {
            background: resolve_color(self.background)?,
            foreground: resolve_color(self.foreground)?,
            cursor: resolve_color(self.cursor)?,
            cursorline: resolve_color(self.cursorline)?,
            statusline: resolve_style(self.statusline)?,
            statusline_inactive: resolve_style(self.statusline_inactive)?,
            gutter: resolve_color(self.gutter)?,
            gutter_current: resolve_color(self.gutter_current)?,
            popup: resolve_style(self.popup)?,
            selection: resolve_style(self.selection)?,
            diagnostic_error: resolve_color(self.diagnostic_error)?,
            diagnostic_warn: resolve_color(self.diagnostic_warn)?,
        })
    }
}

/// Per-character text modifiers.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Modifiers {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strikethrough: bool,
}

/// Foreground, background, and modifier flags for a syntax or UI element.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct StyleSpec {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub modifiers: Modifiers,
}

// ---------------------------------------------------------------------------
// Raw (unresolved) types used during TOML deserialization
// ---------------------------------------------------------------------------

/// Raw `modifiers` array from TOML — strings like `"bold"`, `"italic"`.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RawModifiers(pub Vec<String>);

impl RawModifiers {
    pub(crate) fn resolve(self) -> Result<Modifiers, ThemeError> {
        let mut m = Modifiers::default();
        for s in self.0 {
            match s.as_str() {
                "bold" => m.bold = true,
                "italic" => m.italic = true,
                "underline" => m.underline = true,
                "reverse" => m.reverse = true,
                "strikethrough" => m.strikethrough = true,
                _ => return Err(ThemeError::BadModifier(s)),
            }
        }
        Ok(m)
    }
}

/// Full table form: `{ fg = "...", bg = "...", modifiers = [...] }`.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RawStyleFull {
    pub fg: Option<ColorRef>,
    pub bg: Option<ColorRef>,
    #[serde(default)]
    pub modifiers: RawModifiers,
}

/// Raw `StyleSpec` before palette resolution.
/// TOML shorthand `"#abc123"` or `"$name"` maps to `Shorthand`; table form maps to `Full`.
#[derive(Clone, Debug)]
pub(crate) enum RawStyleSpec {
    Full(RawStyleFull),
    Shorthand(ColorRef),
}

impl<'de> Deserialize<'de> for RawStyleSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Use toml::Value as the intermediate to branch on the TOML type cleanly.
        let v = toml::Value::deserialize(d)?;
        match v {
            toml::Value::String(_) => {
                let cr = ColorRef::deserialize(v).map_err(serde::de::Error::custom)?;
                Ok(RawStyleSpec::Shorthand(cr))
            }
            toml::Value::Table(_) => {
                let full = RawStyleFull::deserialize(v).map_err(serde::de::Error::custom)?;
                Ok(RawStyleSpec::Full(full))
            }
            other => Err(serde::de::Error::custom(format!(
                "expected string or table for style, got {}",
                other.type_str()
            ))),
        }
    }
}

impl RawStyleSpec {
    pub(crate) fn resolve(self, palette: &HashMap<String, Color>) -> Result<StyleSpec, ThemeError> {
        match self {
            RawStyleSpec::Shorthand(cr) => Ok(StyleSpec {
                fg: Some(cr.resolve(palette)?),
                ..Default::default()
            }),
            RawStyleSpec::Full(f) => {
                let fg = f.fg.map(|cr| cr.resolve(palette)).transpose()?;
                let bg = f.bg.map(|cr| cr.resolve(palette)).transpose()?;
                let modifiers = f.modifiers.resolve()?;
                Ok(StyleSpec { fg, bg, modifiers })
            }
        }
    }
}
