use std::{collections::HashMap, path::Path};

use crate::{
    ThemeError,
    captures::CaptureMap,
    color::{Color, RawPalette},
    palette::Palette,
    style::{RawStyleSpec, RawUiStyles, StyleSpec, UiStyles},
};

/// Fully resolved theme.
#[derive(Clone, Default, Debug)]
pub struct Theme {
    /// Resolved palette kept for introspection.
    pub palette: HashMap<String, Color>,
    /// UI surface styles.
    pub ui: UiStyles,
    /// Tree-sitter capture styles with fallback-chain support.
    pub captures: CaptureMap,
}

impl Theme {
    /// Parse a theme from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, ThemeError> {
        // Parse to Value first, then extract sections to avoid toml-crate
        // flatten issues when combining named table keys with catch-all maps.
        let mut table: toml::Table = toml::from_str(s)?;

        // 1. Extract and resolve palette.
        let palette = if let Some(v) = table.remove("palette") {
            let raw: RawPalette = v.try_into()?;
            Palette::from_raw(raw)
        } else {
            Palette::default()
        };

        // 2. Extract ui section.
        let raw_ui: Option<RawUiStyles> = if let Some(v) = table.remove("ui") {
            Some(v.try_into()?)
        } else {
            None
        };
        let ui = raw_ui
            .map(|raw| raw.resolve(&palette.0))
            .transpose()?
            .unwrap_or_default();

        // 3. Remaining keys are capture entries.
        let mut flat: HashMap<String, StyleSpec> = HashMap::new();
        for (key, val) in table {
            let raw: RawStyleSpec = val.try_into()?;
            flat.insert(key, raw.resolve(&palette.0)?);
        }

        Ok(Theme {
            palette: palette.0,
            ui,
            captures: CaptureMap::from_map(flat),
        })
    }

    /// Parse a theme from a file path.
    pub fn from_path(p: &Path) -> Result<Self, ThemeError> {
        let s = std::fs::read_to_string(p)?;
        Self::from_toml_str(&s)
    }
}
