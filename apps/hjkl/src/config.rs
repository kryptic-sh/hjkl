//! User config schema for hjkl.
//!
//! Default values live in [`config.toml`](../config.toml) next to this
//! module and are bundled into the binary at compile time via
//! [`include_str!`]. There are **no default values in Rust code** —
//! every default flows from the bundled TOML so the source tree has a
//! single source of truth.
//!
//! User overrides at `$XDG_CONFIG_HOME/hjkl/config.toml` are deep-merged
//! on top of the bundle via [`hjkl_config::load_layered`]. Only the
//! fields you want to override need to appear in the user file.

use std::path::Path;

use hjkl_config::{
    AppConfig, ConfigError, ConfigSource, Validate, ValidationError, ensure_non_empty_str,
    ensure_non_zero, ensure_range, load_layered, load_layered_from,
};
use serde::Deserialize;

/// Bundled default config — the source of truth for default values.
pub const DEFAULTS_TOML: &str = include_str!("config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub editor: EditorConfig,
    pub theme: ThemeConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorConfig {
    /// Leader key in normal mode. Single character.
    pub leader: char,
    /// Fallback indent width when no `.editorconfig` covers the open file.
    pub tab_width: u8,
    /// Fallback for spaces-vs-tabs when no `.editorconfig` covers the file.
    pub expandtab: bool,
    /// Files with this many lines or more skip per-keystroke git diff recompute.
    pub huge_file_threshold: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    /// Theme name. Currently only `"dark"` is bundled.
    pub name: String,
}

impl Default for Config {
    /// Parses the bundled [`DEFAULTS_TOML`]. Panics if the bundled file is
    /// malformed — that's a build-time bug caught by [`tests::defaults_parse`].
    fn default() -> Self {
        toml::from_str(DEFAULTS_TOML).expect("bundled config.toml is invalid; build-time bug")
    }
}

impl AppConfig for Config {
    const APPLICATION: &'static str = "hjkl";
}

/// Load `Config` by layering the user file (XDG path) over the bundled
/// defaults. Missing user file → bundled-only.
pub fn load() -> Result<(Config, ConfigSource), ConfigError> {
    load_layered::<Config>(DEFAULTS_TOML)
}

/// Load `Config` from an explicit path (for `--config <PATH>` CLI override).
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    load_layered_from::<Config>(DEFAULTS_TOML, path)
}

impl Validate for Config {
    type Error = ValidationError;

    fn validate(&self) -> Result<(), Self::Error> {
        // Multi-char + empty leaders are already rejected by serde's
        // `char` deserializer at parse time (TOML strings of length != 1
        // fail to convert to `char`). We additionally reject control
        // characters here — they parse cleanly but are unbindable: Esc
        // would conflict with mode-exit, NUL/newline can't be typed,
        // etc.
        if self.editor.leader.is_control() {
            return Err(ValidationError::new(
                "editor.leader",
                format!(
                    "must not be a control character (got U+{:04X})",
                    self.editor.leader as u32
                ),
            ));
        }
        ensure_range(self.editor.tab_width, 1, 16, "editor.tab_width")?;
        ensure_non_zero(
            self.editor.huge_file_threshold,
            "editor.huge_file_threshold",
        )?;
        // Empty theme.name is meaningless. Unknown *non-empty* names still
        // fall back to "dark" with a runtime warning (permissive rollout
        // for future themes), but `name = ""` indicates a config bug, not
        // a forward-compat unknown.
        ensure_non_empty_str(&self.theme.name, "theme.name")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-time check: the bundled defaults must parse into `Config`.
    /// If this fails, `Config::default()` would panic at runtime.
    #[test]
    fn defaults_parse() {
        let cfg: Config = toml::from_str(DEFAULTS_TOML).expect("bundled config.toml must parse");
        // Sanity-check field shape — guards against silent schema drift.
        assert_eq!(cfg.editor.leader, ' ');
        assert_eq!(cfg.editor.tab_width, 4);
        assert!(cfg.editor.expandtab);
        assert_eq!(cfg.editor.huge_file_threshold, 50_000);
        assert_eq!(cfg.theme.name, "dark");
    }

    #[test]
    fn defaults_match_default_impl() {
        let parsed: Config = toml::from_str(DEFAULTS_TOML).unwrap();
        let dflt = Config::default();
        assert_eq!(parsed.editor.leader, dflt.editor.leader);
        assert_eq!(parsed.editor.tab_width, dflt.editor.tab_width);
        assert_eq!(parsed.theme.name, dflt.theme.name);
    }

    #[test]
    fn user_partial_override_keeps_defaults() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nleader = \"\\\\\"").unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(cfg.editor.leader, '\\');
        assert_eq!(
            cfg.editor.tab_width, 4,
            "non-overridden field keeps default"
        );
        assert_eq!(
            cfg.theme.name, "dark",
            "non-overridden section keeps default"
        );
    }

    #[test]
    fn unknown_user_key_is_rejected() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nbogus = 1").unwrap();
        assert!(load_from(f.path()).is_err());
    }

    #[test]
    fn defaults_pass_validation() {
        Config::default()
            .validate()
            .expect("bundled defaults must validate");
    }

    #[test]
    fn validate_rejects_zero_tab_width() {
        let mut cfg = Config::default();
        cfg.editor.tab_width = 0;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "editor.tab_width");
    }

    #[test]
    fn validate_rejects_huge_tab_width() {
        let mut cfg = Config::default();
        cfg.editor.tab_width = 64;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "editor.tab_width");
        assert!(err.message.contains("64"));
    }

    #[test]
    fn validate_accepts_tab_width_boundary() {
        let mut cfg = Config::default();
        cfg.editor.tab_width = 1;
        assert!(cfg.validate().is_ok());
        cfg.editor.tab_width = 16;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_huge_file_threshold() {
        let mut cfg = Config::default();
        cfg.editor.huge_file_threshold = 0;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "editor.huge_file_threshold");
    }

    /// Multi-char leader strings must be rejected at parse time — serde's
    /// `char` deserializer fails on TOML strings of length != 1. This pins
    /// the contract: users who write `leader = "ab"` or `leader = "<C-x>"`
    /// get a `ConfigError::Invalid` (post-merge type-check), not a silently
    /// truncated leader.
    #[test]
    fn parse_rejects_multi_char_leader() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nleader = \"ab\"").unwrap();
        let err = load_from(f.path()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { .. }),
            "expected Invalid for multi-char leader, got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_empty_leader() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nleader = \"\"").unwrap();
        let err = load_from(f.path()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Invalid { .. }),
            "expected Invalid for empty leader, got {err:?}"
        );
    }

    #[test]
    fn validate_rejects_control_char_leader() {
        // Esc (U+001B) parses cleanly as a single char but would clash
        // with mode-exit semantics — reject at validation time.
        let mut cfg = Config::default();
        cfg.editor.leader = '\x1b';
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "editor.leader");
        assert!(err.message.contains("control"));
    }

    #[test]
    fn validate_accepts_common_leader_chars() {
        for c in [' ', '\\', ',', ';', 'a'] {
            let mut cfg = Config::default();
            cfg.editor.leader = c;
            assert!(cfg.validate().is_ok(), "leader {c:?} should be accepted");
        }
    }

    #[test]
    fn validate_rejects_empty_theme_name() {
        let mut cfg = Config::default();
        cfg.theme.name = String::new();
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "theme.name");
    }
}
