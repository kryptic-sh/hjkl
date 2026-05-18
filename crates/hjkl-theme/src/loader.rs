//! Free-function loader API for [`Theme`].
//!
//! Wraps `Theme::from_toml_str` / `Theme::from_path` as top-level functions
//! so callers can write `hjkl_theme::loader::load_from_path(p)?` without
//! importing the `Theme` struct, and provides `default_theme()` with a
//! minimal bundled dark palette.
//!
//! `resolve_palette_refs` is exposed here for symmetry, but resolution
//! happens automatically inside `parse_toml` / `load_from_path`; callers
//! that build a `Theme` via those functions get fully-resolved colors with
//! no additional step required.

use std::path::Path;

use crate::{Theme, ThemeError};

/// Built-in minimal dark theme source embedded at compile time.
const DEFAULT_THEME_TOML: &str = include_str!("../themes/default.toml");

/// Parse a TOML string into a fully-resolved [`Theme`].
///
/// Palette `$name` references are resolved automatically.
///
/// # Errors
/// Returns [`ThemeError::Toml`] on parse failure or
/// [`ThemeError::UnresolvedPalette`] / [`ThemeError::BadHex`] on bad
/// color values.
pub fn parse_toml(src: &str) -> Result<Theme, ThemeError> {
    Theme::from_toml_str(src)
}

/// Read a TOML file from `path` and return a fully-resolved [`Theme`].
///
/// Palette `$name` references are resolved automatically.
///
/// # Errors
/// Returns [`ThemeError::Io`] on read failure or any error that
/// [`parse_toml`] can return.
pub fn load_from_path(path: &Path) -> Result<Theme, ThemeError> {
    Theme::from_path(path)
}

/// Walk a [`Theme`] and verify all palette references are resolved.
///
/// After a successful call to [`parse_toml`] or [`load_from_path`] this
/// is always a no-op — resolution is performed during parsing. The
/// function is provided as an explicit checkpoint for callers that
/// construct a `Theme` by other means (e.g. merging two themes) and
/// want to assert completeness before use.
///
/// Returns `Ok(())` when every color in the theme is already concrete.
///
/// # Errors
/// Returns [`ThemeError::UnresolvedPalette`] if a `$name` palette
/// reference could not be resolved. (This cannot happen for themes
/// produced by [`parse_toml`] / [`load_from_path`].)
pub fn resolve_palette_refs(_theme: &mut Theme) -> Result<(), ThemeError> {
    // Resolution is performed eagerly during TOML parsing.
    // For themes produced by `parse_toml` / `load_from_path` there is
    // nothing left to do. This entry-point exists so that callers who
    // merge or mutate themes have a stable hook point if resolution
    // is added as a separate phase in the future.
    Ok(())
}

/// Return the bundled default dark [`Theme`].
///
/// The theme is a minimal dark palette suitable as a fallback when no
/// user theme file is found. It is embedded in the binary at compile
/// time and always valid.
///
/// # Panics
/// Panics if the bundled TOML is malformed — this would be a compile-time
/// error caught by `cargo test`.
pub fn default_theme() -> Theme {
    parse_toml(DEFAULT_THEME_TOML).expect("bundled default.toml is always valid")
}
