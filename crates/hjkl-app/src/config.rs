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
    ensure_range, load_layered, load_layered_from,
};
use serde::Deserialize;

/// Bundled default config — the source of truth for default values.
pub const DEFAULTS_TOML: &str = include_str!("config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub editor: EditorConfig,
    pub theme: ThemeConfig,
    #[serde(default)]
    pub lsp: hjkl_lsp::LspConfig,
    #[serde(default)]
    pub which_key: WhichKeyConfig,
    /// Left file-explorer dock sizing (window-management refactor Phase A).
    #[serde(default)]
    pub explorer: ExplorerDockConfig,
    /// Bottom panel dock sizing. Unused until the quickfix/location-list
    /// dock lands (Phase B); plumbed now so the config schema is stable.
    #[serde(default)]
    pub panel: PanelConfig,
}

/// Sizing for the left file-explorer dock (Phase A of the window-management
/// refactor). The dock lives outside the per-tab `LayoutTree` — see
/// `apps/hjkl/src/app/dock.rs` — so its size is config-driven rather than a
/// split ratio. Interactive resize (`<C-w><`/`<C-w>>`, border-drag) writes
/// this value back to the user's config file via `hjkl_config::write_key_at`
/// so it survives across sessions.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerDockConfig {
    /// Width in terminal columns. Runtime-clamped to a sane range against
    /// the current terminal width (see `dock::clamp_dock_width`); this is
    /// just the persisted preference.
    pub width: u16,
    /// Whether the explorer dock is open. Written back on every
    /// `toggle_explorer` (`<leader>e`, `<C-w>c` on the dock) and read on
    /// startup to reopen the dock if it was left open (#63 Phase C).
    /// `#[serde(default)]` so a hand-edited `[explorer]` section that
    /// predates this field (only `width` set) still parses.
    #[serde(default)]
    pub open: bool,
}

impl Default for ExplorerDockConfig {
    fn default() -> Self {
        Self {
            width: 36,
            open: false,
        }
    }
}

/// Sizing for the bottom panel dock (Phase B: quickfix / location-list).
/// Reserved and plumbed in Phase A; nothing renders into it yet.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelConfig {
    /// Height in terminal rows.
    pub height: u16,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self { height: 12 }
    }
}

/// Configuration for the which-key popup.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhichKeyConfig {
    /// Whether the which-key popup is enabled.
    pub enabled: bool,
    /// Idle delay in milliseconds before the popup appears.
    pub delay_ms: u64,
}

impl Default for WhichKeyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 500,
        }
    }
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
    /// Whether mouse capture (and wheel-scrolls-viewport) is on at startup.
    /// Runtime-togglable via `:set [no]mouse`.
    pub mouse: bool,
    /// Milliseconds to wait for a chord to complete before timing out.
    /// Vim's `:set timeoutlen` equivalent. Default 1000.
    ///
    /// Must be strictly greater than `which_key.delay_ms`; if it is not,
    /// a startup warning is emitted and the chord-resolve race described
    /// in the `App::with_config` doc comment may re-emerge.
    pub chord_timeout_ms: u64,
    /// Icon set for the file explorer: `"nerd"`, `"unicode"`, `"ascii"`, or
    /// `"auto"`. Terminals can't be queried for their font, so `auto` assumes a
    /// Nerd Font; `unicode`/`ascii` are the reliable non-Nerd fallbacks (see
    /// `hjkl-icons`). Defaults to `"auto"` so existing configs keep parsing.
    #[serde(default = "default_icons")]
    pub icons: String,
    /// Cross-session cursor memory (shada-style): when `true`, reopening a file
    /// restores the last cursor position on that buffer, clamped to the current
    /// content. When `false`, the state store
    /// is neither read nor written. Default `true`. `#[serde(default)]` so
    /// configs predating this field keep parsing.
    #[serde(default = "default_restore_cursor")]
    pub restore_cursor: bool,
    /// Persistent undo (`undofile`): when `true`, `:w` writes the whole undo
    /// tree to disk and reopening the same, unchanged file restores the exact
    /// node it was saved on so `u`/`<C-r>` walk across the session boundary
    /// Content-hash gated: an external change
    /// discards the stored tree. When `false`, the undofile is neither read nor
    /// written. Default `true`. `#[serde(default)]` so older configs still
    /// parse.
    #[serde(default = "default_undofile")]
    pub undofile: bool,
    /// Optional override for the undofile directory. When unset, undofiles live
    /// under `<XDG_STATE_HOME>/hjkl/undo/`. `#[serde(default)]` so older configs
    /// still parse.
    #[serde(default)]
    pub undodir: Option<String>,
}

fn default_icons() -> String {
    "auto".to_string()
}

fn default_restore_cursor() -> bool {
    true
}

fn default_undofile() -> bool {
    true
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
        // Static sanity bounds; the runtime clamp against the live terminal
        // width (`dock::clamp_dock_width`) is a separate, dynamic check.
        ensure_range(self.explorer.width, 12, 400, "explorer.width")?;
        ensure_range(self.panel.height, 3, 200, "panel.height")?;
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
        assert!(cfg.editor.mouse, "mouse defaults on");
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

    // ── chord_timeout_ms tests ─────────────────────────────────────────────

    /// Bundled default must set chord_timeout_ms = 1000.
    #[test]
    fn defaults_chord_timeout_ms_is_1000() {
        let cfg: Config = toml::from_str(DEFAULTS_TOML).expect("bundled config.toml must parse");
        assert_eq!(cfg.editor.chord_timeout_ms, 1000);
    }

    /// A user config that explicitly sets chord_timeout_ms must override the
    /// bundled default.
    #[test]
    fn user_override_chord_timeout_ms() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nchord_timeout_ms = 250").unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(cfg.editor.chord_timeout_ms, 250);
        // Non-overridden fields must keep their bundled defaults.
        assert_eq!(cfg.editor.tab_width, 4, "tab_width must keep default");
    }

    /// A user config that omits chord_timeout_ms must inherit the bundled default.
    #[test]
    fn user_partial_override_keeps_chord_timeout_ms_default() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nleader = \"\\\\\"").unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(
            cfg.editor.chord_timeout_ms, 1000,
            "chord_timeout_ms must keep default when not overridden"
        );
    }

    // ── dock config (explorer.width / panel.height) ─────────────────────────

    /// Bundled default must set explorer.width = 36 (matches the previous
    /// `EXPLORER_WINDOW_WIDTH` constant) and panel.height = 12.
    #[test]
    fn defaults_dock_sizes() {
        let cfg: Config = toml::from_str(DEFAULTS_TOML).expect("bundled config.toml must parse");
        assert_eq!(cfg.explorer.width, 36);
        assert_eq!(cfg.panel.height, 12);
    }

    #[test]
    fn user_override_explorer_width() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[explorer]\nwidth = 50").unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(cfg.explorer.width, 50);
        assert_eq!(cfg.panel.height, 12, "panel.height must keep default");
    }

    #[test]
    fn validate_rejects_too_narrow_explorer_width() {
        let mut cfg = Config::default();
        cfg.explorer.width = 1;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "explorer.width");
    }

    #[test]
    fn validate_rejects_zero_panel_height() {
        let mut cfg = Config::default();
        cfg.panel.height = 0;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "panel.height");
    }
}
