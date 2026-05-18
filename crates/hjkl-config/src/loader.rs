use std::env;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{ConfigError, locate};

/// Application config descriptor.
///
/// Implementing types are loaded from a TOML file under the application's
/// XDG config directory. The trait carries identity ([`APPLICATION`]) and
/// the per-impl filename ([`FILE`]) used by the load functions in this
/// crate.
///
/// Multiple structs can impl `AppConfig` with the same `APPLICATION` and
/// different `FILE` constants — that's how an app splits its user config
/// across e.g. `config.toml`, `keymap.toml`, `theme.toml`. For raw
/// directory lookups not tied to a specific struct, use the free
/// functions [`config_dir`], [`data_dir`], [`cache_dir`].
///
/// [`APPLICATION`]: AppConfig::APPLICATION
/// [`FILE`]: AppConfig::FILE
pub trait AppConfig: DeserializeOwned + Default {
    /// Application name. Becomes the leaf component of the XDG dirs:
    /// `$XDG_CONFIG_HOME/<APPLICATION>/`, `$XDG_DATA_HOME/<APPLICATION>/`, etc.
    const APPLICATION: &'static str;
    /// File basename for this struct's TOML, joined onto the config dir.
    /// Defaults to `config.toml`. Override per-impl when an app splits its
    /// user config across multiple files (e.g. `keymap.toml`).
    const FILE: &'static str = "config.toml";
}

/// Where the loaded config came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Parsed from this user file.
    File(PathBuf),
    /// No user file present; built-in `Default` was used.
    Defaults,
}

// ── XDG-everywhere resolver ────────────────────────────────────────────────
//
// Per the XDG Base Directory spec, each base var (XDG_CONFIG_HOME,
// XDG_DATA_HOME, XDG_CACHE_HOME) is honored if set to a non-empty
// absolute path; otherwise the documented default applies. This is
// applied uniformly on every platform — Linux, macOS, Windows — so a
// user's config / data / cache layout is identical everywhere.
//
// macOS users get `~/.config/<app>` instead of
// `~/Library/Application Support/<app>` because we ship CLI tools, not
// signed `.app` bundles. Windows users get `~/.config/<app>` instead of
// `%APPDATA%\<app>` for the same reason.

/// Pure XDG resolver — no I/O, no env access. Takes the would-be env
/// value and home-dir lookup as inputs so tests can exercise every
/// branch without `std::env::set_var` (which races across parallel
/// tests and is hard to make cross-platform-correct: a Unix-style
/// `/tmp/foo` path returns `false` from `Path::is_absolute()` on
/// Windows, and vice versa).
fn resolve_xdg(
    env_value: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
    fallback_subdir: &str,
) -> Result<PathBuf, ConfigError> {
    if let Some(raw) = env_value
        && !raw.is_empty()
    {
        let p = PathBuf::from(raw);
        // XDG spec: relative paths are ignored; only absolute paths count.
        if p.is_absolute() {
            return Ok(p);
        }
    }
    home.map(|h| h.join(fallback_subdir))
        .ok_or(ConfigError::NoHomeDir)
}

fn xdg_base(env_var: &str, fallback_subdir: &str) -> Result<PathBuf, ConfigError> {
    let env_value = env::var_os(env_var);
    let home = dirs::home_dir();
    resolve_xdg(env_value.as_deref(), home.as_deref(), fallback_subdir)
}

/// Resolve `<XDG_CONFIG_HOME>/<app>`, defaulting to `~/.config/<app>`.
pub fn config_dir(app: &str) -> Result<PathBuf, ConfigError> {
    Ok(xdg_base("XDG_CONFIG_HOME", ".config")?.join(app))
}

/// Resolve `<XDG_DATA_HOME>/<app>`, defaulting to `~/.local/share/<app>`.
pub fn data_dir(app: &str) -> Result<PathBuf, ConfigError> {
    Ok(xdg_base("XDG_DATA_HOME", ".local/share")?.join(app))
}

/// Resolve `<XDG_CACHE_HOME>/<app>`, defaulting to `~/.cache/<app>`.
pub fn cache_dir(app: &str) -> Result<PathBuf, ConfigError> {
    Ok(xdg_base("XDG_CACHE_HOME", ".cache")?.join(app))
}

/// Resolve the full config file path for `C` — `<config_dir>/<FILE>`.
pub fn config_path<C: AppConfig>() -> Result<PathBuf, ConfigError> {
    Ok(config_dir(C::APPLICATION)?.join(C::FILE))
}

/// Load `C` from its default XDG path.
///
/// Returns `(C::default(), ConfigSource::Defaults)` if the file is absent
/// or the platform has no home dir. Never writes to disk.
pub fn load<C: AppConfig>() -> Result<(C, ConfigSource), ConfigError> {
    let path = match config_path::<C>() {
        Ok(p) => p,
        Err(ConfigError::NoHomeDir) => {
            return Ok((C::default(), ConfigSource::Defaults));
        }
        Err(e) => return Err(e),
    };
    if !path.exists() {
        return Ok((C::default(), ConfigSource::Defaults));
    }
    let cfg = load_from::<C>(&path)?;
    Ok((cfg, ConfigSource::File(path)))
}

/// Load `C` from an explicit path. Used by `--config <PATH>` flags and tests.
///
/// Errors with [`ConfigError::Io`] on read failure or [`ConfigError::Parse`]
/// (with line/col/snippet) on malformed TOML.
pub fn load_from<C: AppConfig>(path: &Path) -> Result<C, ConfigError> {
    let src = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    toml::from_str::<C>(&src).map_err(|e| {
        let span = e.span().unwrap_or(0..0);
        let (line, col, snippet) = locate(&src, span.start);
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: e.message().to_string(),
            snippet,
        }
    })
}

/// Load `C` by layering a user file over a bundled defaults TOML.
///
/// `defaults_toml` is parsed as the seed value (typically embedded via
/// `include_str!()` so default *values* live in a TOML file in the source
/// tree, not in Rust code). If a user file exists at `C`'s default XDG
/// path, it is parsed and **deep-merged** on top of the seed: nested
/// tables are merged recursively; scalars and arrays in the user file
/// overwrite their seed counterparts.
///
/// When no user file exists, returns the seed-only value paired with
/// [`ConfigSource::Defaults`]. Never writes to disk.
///
/// Errors:
/// - [`ConfigError::Invalid`] if `defaults_toml` itself is malformed or
///   doesn't deserialize into `C` — this is a build-time bug in the
///   consumer, not a user error.
/// - [`ConfigError::Parse`] with line/col/snippet if the user file is
///   malformed TOML.
/// - [`ConfigError::Invalid`] if the merged result fails to deserialize
///   into `C` (unknown user key, wrong type, etc).
pub fn load_layered<C: AppConfig>(defaults_toml: &str) -> Result<(C, ConfigSource), ConfigError> {
    let user_path = match config_path::<C>() {
        Ok(p) if p.exists() => Some(p),
        Ok(_) => None,
        Err(ConfigError::NoHomeDir) => None,
        Err(e) => return Err(e),
    };
    match user_path {
        Some(p) => {
            let cfg = load_layered_from::<C>(defaults_toml, &p)?;
            Ok((cfg, ConfigSource::File(p)))
        }
        None => {
            let cfg = parse_defaults_only::<C>(defaults_toml)?;
            Ok((cfg, ConfigSource::Defaults))
        }
    }
}

/// Same as [`load_layered`] but reads the user file from an explicit path
/// (for `--config <PATH>` flags and tests). Always reads `path` — does not
/// fall back to defaults if missing.
pub fn load_layered_from<C: AppConfig>(defaults_toml: &str, path: &Path) -> Result<C, ConfigError> {
    let user_src = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let user_table: toml::Table = toml::from_str(&user_src).map_err(|e| {
        let span = e.span().unwrap_or(0..0);
        let (line, col, snippet) = locate(&user_src, span.start);
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: e.message().to_string(),
            snippet,
        }
    })?;

    let mut merged: toml::Table =
        toml::from_str(defaults_toml).map_err(|e| ConfigError::Invalid {
            path: PathBuf::from("<bundled defaults>"),
            message: format!("bundled defaults TOML is invalid: {e}"),
        })?;
    deep_merge(&mut merged, user_table);

    toml::Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

fn parse_defaults_only<C: AppConfig>(defaults_toml: &str) -> Result<C, ConfigError> {
    toml::from_str::<C>(defaults_toml).map_err(|e| ConfigError::Invalid {
        path: PathBuf::from("<bundled defaults>"),
        message: format!("bundled defaults TOML is invalid: {e}"),
    })
}

/// Recursively merge `from` into `into`. Nested tables merge field-by-field;
/// scalars and arrays in `from` overwrite their counterparts in `into`.
pub(crate) fn deep_merge(into: &mut toml::Table, from: toml::Table) {
    for (k, v) in from {
        match (into.get_mut(&k), v) {
            (Some(toml::Value::Table(into_t)), toml::Value::Table(from_t)) => {
                deep_merge(into_t, from_t);
            }
            (_, v) => {
                into.insert(k, v);
            }
        }
    }
}

/// Serialize `cfg` and write it to `path`, creating parent directories.
///
/// Opt-in helper for apps that want to scaffold a starter config on user
/// request. **Not** called automatically by [`load`] — callers must invoke
/// it explicitly (e.g. behind a `--init` flag).
pub fn write_default<C: AppConfig + Serialize>(path: &Path, cfg: &C) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Write {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let body = toml::to_string_pretty(cfg).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    })?;
    std::fs::write(path, body).map_err(|e| ConfigError::Write {
        path: path.to_path_buf(),
        source: e,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    /// `tempfile::tempdir()` gives an absolute path that's well-formed
    /// on every platform. We use it as the env-value input so the
    /// `is_absolute()` branch fires consistently on Windows + Unix.
    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    /// XDG var set to a valid absolute path → `<that>/<subdir-ignored>`.
    /// Resolver returns the env value as-is (sub-dir param is the
    /// fallback, not used when the env path wins).
    #[test]
    fn xdg_set_absolute_wins_over_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let env = os(tmp.path().to_str().unwrap());
        let home = std::path::PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, tmp.path());
    }

    /// XDG var set to empty → fallback to `<home>/<subdir>`.
    #[test]
    fn xdg_empty_falls_back_to_home() {
        let env = os("");
        let home = std::path::PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".config").unwrap();
        assert_eq!(r, std::path::PathBuf::from("/some/home/.config"));
    }

    /// XDG var unset → fallback to `<home>/<subdir>`.
    #[test]
    fn xdg_unset_falls_back_to_home() {
        let home = std::path::PathBuf::from("/some/home");
        let r = resolve_xdg(None, Some(&home), ".local/share").unwrap();
        assert_eq!(r, std::path::PathBuf::from("/some/home/.local/share"));
    }

    /// XDG var set to a relative path → ignored per spec, fall back.
    #[test]
    fn xdg_relative_ignored_per_spec() {
        let env = os("relative/path");
        let home = std::path::PathBuf::from("/some/home");
        let r = resolve_xdg(Some(&env), Some(&home), ".cache").unwrap();
        assert_eq!(r, std::path::PathBuf::from("/some/home/.cache"));
    }

    /// No XDG var, no home dir → `NoHomeDir` error. Sandboxed test
    /// environments are the only realistic trigger.
    #[test]
    fn xdg_no_env_no_home_errs() {
        let r = resolve_xdg(None, None, ".config");
        assert!(matches!(r, Err(ConfigError::NoHomeDir)));
    }

    /// Smoke test the public `config_dir(app)` returns *something*
    /// ending in `<app>` — exact path depends on the test runner's
    /// $HOME, so we just check the leaf component.
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
