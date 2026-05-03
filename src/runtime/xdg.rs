//! XDG Base Directory resolution for grammar storage.
//!
//! Honors `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` on every platform with
//! `~/.local/share` / `~/.cache` fallbacks. macOS and Windows do *not*
//! get their platform-native paths (`~/Library/Application Support`,
//! `%APPDATA%`) — bonsai stores grammar caches uniformly across
//! platforms so a `~/.local/share/bonsai/` checkout looks identical on
//! every machine.
//!
//! Self-contained so `hjkl-bonsai` can be used without pulling
//! `hjkl-config` / the rest of the hjkl umbrella.

use std::path::PathBuf;

use anyhow::{Context, Result};

fn resolve(env_var: &str, fallback_subdir: &str) -> Result<PathBuf> {
    if let Some(raw) = std::env::var_os(env_var)
        && !raw.is_empty()
    {
        let p = PathBuf::from(&raw);
        if p.is_absolute() {
            return Ok(p);
        }
    }
    let home = dirs::home_dir().context("could not resolve $HOME")?;
    Ok(home.join(fallback_subdir))
}

/// `$XDG_DATA_HOME` or `~/.local/share`.
pub fn data_home() -> Result<PathBuf> {
    resolve("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_home() -> Result<PathBuf> {
    resolve("XDG_CACHE_HOME", ".cache")
}
