use serde::{Deserialize, Serialize};

use crate::ThemeError;

/// Resolved RGBA color (all channels 0–255).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Construct from RGB; alpha defaults to 255.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Construct from RGBA.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Parse `#rgb`, `#rrggbb`, or `#rrggbbaa`.
    pub fn from_hex_str(s: &str) -> Result<Self, ThemeError> {
        let s = s
            .strip_prefix('#')
            .ok_or_else(|| ThemeError::BadHex(s.to_owned()))?;
        fn hex2(b: &[u8]) -> Option<u8> {
            let hi = (b[0] as char).to_digit(16)? as u8;
            let lo = (b[1] as char).to_digit(16)? as u8;
            Some(hi << 4 | lo)
        }
        fn expand(nibble: u8) -> u8 {
            nibble << 4 | nibble
        }
        match s.len() {
            3 => {
                let b = s.as_bytes();
                let r = (b[0] as char).to_digit(16).map(|n| expand(n as u8));
                let g = (b[1] as char).to_digit(16).map(|n| expand(n as u8));
                let b_ = (b[2] as char).to_digit(16).map(|n| expand(n as u8));
                match (r, g, b_) {
                    (Some(r), Some(g), Some(b)) => Ok(Color::rgb(r, g, b)),
                    _ => Err(ThemeError::BadHex(format!("#{s}"))),
                }
            }
            6 => {
                let b = s.as_bytes();
                match (hex2(&b[0..2]), hex2(&b[2..4]), hex2(&b[4..6])) {
                    (Some(r), Some(g), Some(b)) => Ok(Color::rgb(r, g, b)),
                    _ => Err(ThemeError::BadHex(format!("#{s}"))),
                }
            }
            8 => {
                let b = s.as_bytes();
                match (
                    hex2(&b[0..2]),
                    hex2(&b[2..4]),
                    hex2(&b[4..6]),
                    hex2(&b[6..8]),
                ) {
                    (Some(r), Some(g), Some(b), Some(a)) => Ok(Color::rgba(r, g, b, a)),
                    _ => Err(ThemeError::BadHex(format!("#{s}"))),
                }
            }
            _ => Err(ThemeError::BadHex(format!("#{s}"))),
        }
    }
}

/// Raw color value in TOML: either a literal hex string or a `$palette_name` ref.
#[derive(Clone, Debug)]
pub(crate) enum ColorRef {
    Palette(String),
    Literal(Color),
}

impl<'de> Deserialize<'de> for ColorRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if let Some(name) = s.strip_prefix('$') {
            Ok(ColorRef::Palette(name.to_owned()))
        } else {
            Color::from_hex_str(&s)
                .map(ColorRef::Literal)
                .map_err(serde::de::Error::custom)
        }
    }
}

/// Newtype for a hex-literal color in the palette table (no `$` refs allowed there).
#[derive(Clone, Debug)]
pub(crate) struct LiteralColor(pub Color);

impl<'de> Deserialize<'de> for LiteralColor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::from_hex_str(&s)
            .map(LiteralColor)
            .map_err(serde::de::Error::custom)
    }
}

impl ColorRef {
    /// Resolve against the palette. Returns `ThemeError::UnresolvedPalette` on missing name.
    pub(crate) fn resolve(
        self,
        palette: &std::collections::HashMap<String, Color>,
    ) -> Result<Color, ThemeError> {
        match self {
            ColorRef::Literal(c) => Ok(c),
            ColorRef::Palette(name) => palette
                .get(&name)
                .copied()
                .ok_or(ThemeError::UnresolvedPalette(name)),
        }
    }
}

/// Raw palette deserialization: values are hex strings only (no `$` refs in palette itself).
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RawPalette(pub std::collections::HashMap<String, LiteralColor>);
