//! XDG Base Directory resolution.
//!
//! Honors `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME` / `$XDG_CACHE_HOME` on
//! every platform when set to an absolute path. Falls back to
//! `~/.config` / `~/.local/share` / `~/.cache` on every platform —
//! including macOS and Windows. The point is uniformity: bonsai-style
//! standalone consumers must produce identical layouts cross-platform,
//! so we deliberately *do not* use platform-native dirs
//! (`~/Library/Application Support`, `%APPDATA%`).

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("could not resolve $HOME")]
    NoHomeDir,
}

/// Pure XDG resolver — no I/O, no env access.
///
/// Takes the would-be env value and home-dir as inputs so tests can exercise
/// every branch without `std::env::set_var` (which races across parallel
/// tests).
fn resolve_xdg(
    env_value: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
    fallback_subdir: &str,
) -> Result<PathBuf, Error> {
    if let Some(raw) = env_value
        && !raw.is_empty()
    {
        let p = PathBuf::from(raw);
        if p.is_absolute() {
            return Ok(p);
        }
    }
    home.map(|h| h.join(fallback_subdir))
        .ok_or(Error::NoHomeDir)
}

fn resolve(env_var: &str, fallback_subdir: &str) -> Result<PathBuf, Error> {
    let env_value = std::env::var_os(env_var);
    let home = dirs::home_dir();
    resolve_xdg(env_value.as_deref(), home.as_deref(), fallback_subdir)
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> Result<PathBuf, Error> {
    resolve("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME` or `~/.local/share`.
pub fn data_home() -> Result<PathBuf, Error> {
    resolve("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_home() -> Result<PathBuf, Error> {
    resolve("XDG_CACHE_HOME", ".cache")
}

/// Resolve `<XDG_CONFIG_HOME>/<app>`, defaulting to `~/.config/<app>`.
pub fn config_dir(app: &str) -> Result<PathBuf, Error> {
    Ok(config_home()?.join(app))
}

/// Resolve `<XDG_DATA_HOME>/<app>`, defaulting to `~/.local/share/<app>`.
pub fn data_dir(app: &str) -> Result<PathBuf, Error> {
    Ok(data_home()?.join(app))
}

/// Resolve `<XDG_CACHE_HOME>/<app>`, defaulting to `~/.cache/<app>`.
pub fn cache_dir(app: &str) -> Result<PathBuf, Error> {
    Ok(cache_home()?.join(app))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    // ── resolve_xdg unit tests (no env access — parallel-safe) ────────────

    #[test]
    fn xdg_set_absolute_wins_over_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let env = os(tmp.path().to_str().unwrap());
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, tmp.path());
    }

    #[test]
    fn xdg_empty_falls_back_to_home() {
        let env = os("");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.config"));
    }

    #[test]
    fn xdg_unset_falls_back_to_home() {
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(None, Some(&home), ".local/share").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.local/share"));
    }

    #[test]
    fn xdg_relative_ignored_per_spec() {
        let env = os("relative/path");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".cache").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.cache"));
    }

    #[test]
    fn xdg_no_env_no_home_errs() {
        let r = resolve_xdg(None, None, ".config");
        assert!(matches!(r, Err(Error::NoHomeDir)));
    }

    // ── data variants via resolve_xdg ─────────────────────────────────────

    #[test]
    fn xdg_data_home_honors_env_var() {
        let env = os("/tmp/foo");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".local/share").unwrap();
        assert_eq!(r, PathBuf::from("/tmp/foo"));
    }

    #[test]
    fn xdg_data_home_falls_back_when_unset() {
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(None, Some(&home), ".local/share").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.local/share"));
    }

    #[test]
    fn xdg_data_home_ignores_relative_path() {
        let env = os("relative");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".local/share").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.local/share"));
    }

    // ── config variants via resolve_xdg ───────────────────────────────────

    #[test]
    fn xdg_config_home_honors_env_var() {
        let env = os("/tmp/cfghome");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, PathBuf::from("/tmp/cfghome"));
    }

    #[test]
    fn xdg_config_home_falls_back_when_unset() {
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(None, Some(&home), ".config").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.config"));
    }

    #[test]
    fn xdg_config_home_ignores_relative_path() {
        let env = os("relative");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.config"));
    }

    // ── cache variants via resolve_xdg ────────────────────────────────────

    #[test]
    fn xdg_cache_home_honors_env_var() {
        let env = os("/tmp/cachehome");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".cache").unwrap();
        assert_eq!(r, PathBuf::from("/tmp/cachehome"));
    }

    #[test]
    fn xdg_cache_home_falls_back_when_unset() {
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(None, Some(&home), ".cache").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.cache"));
    }

    #[test]
    fn xdg_cache_home_ignores_relative_path() {
        let env = os("relative");
        let home = PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".cache").unwrap();
        assert_eq!(r, PathBuf::from("/some/home/.cache"));
    }

    // ── dir wrappers ──────────────────────────────────────────────────────

    #[test]
    fn xdg_data_dir_appends_app() {
        let env = os("/tmp/foo");
        let home = PathBuf::from("/some/home");
        let base = resolve_xdg(Some(&env), Some(&home), ".local/share").unwrap();
        assert_eq!(base.join("bar"), PathBuf::from("/tmp/foo/bar"));
    }

    // ── smoke tests against live env ──────────────────────────────────────

    #[test]
    fn config_dir_smoke() {
        let p = config_dir("myapp").unwrap();
        assert!(
            p.ends_with("myapp"),
            "expected path ending in `myapp`, got {p:?}"
        );
    }

    #[test]
    fn data_dir_smoke() {
        let p = data_dir("myapp").unwrap();
        assert!(p.ends_with("myapp"));
    }

    #[test]
    fn cache_dir_smoke() {
        let p = cache_dir("myapp").unwrap();
        assert!(p.ends_with("myapp"));
    }
}
