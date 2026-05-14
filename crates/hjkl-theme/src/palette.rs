use std::collections::HashMap;

use crate::color::{Color, LiteralColor, RawPalette};

/// Resolved palette: name -> `Color`.
#[derive(Clone, Default, Debug)]
pub struct Palette(pub HashMap<String, Color>);

impl Palette {
    /// Build from a raw deserialized palette table.
    pub(crate) fn from_raw(raw: RawPalette) -> Self {
        let map = raw
            .0
            .into_iter()
            .map(|(k, LiteralColor(c))| (k, c))
            .collect();
        Self(map)
    }

    /// Look up a color by name.
    pub fn get(&self, name: &str) -> Option<Color> {
        self.0.get(name).copied()
    }
}
