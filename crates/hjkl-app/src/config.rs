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
    ensure_one_of, ensure_range, load_layered, load_layered_from,
};
use serde::Deserialize;

/// Bundled default config — the source of truth for default values.
pub const DEFAULTS_TOML: &str = include_str!("config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub editor: EditorConfig,
    /// Every global `:set` option, named exactly as `:set` names it.
    ///
    /// The 1:1 naming is load-bearing in both directions: it is how a config
    /// key is found for a `:set` token when the value is written back
    /// ([`options_key_for_setting`]), and how a reader knows which key to edit
    /// after discovering an option interactively.
    pub options: OptionsConfig,
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

/// Startup values for every **global** `:set` option.
///
/// Field names match the canonical `:set` name exactly (`:set scrolloff=8` ↔
/// `options.scrolloff = 8`), which is what lets `:set` write itself back to the
/// user's file without a hand-maintained name table — see
/// [`options_key_for_setting`].
///
/// # What is deliberately absent
///
/// The **buffer-local** options are not here and never persist:
/// `filetype`, `commentstring`, `readonly`, `modifiable`, and `endofline`.
/// Each describes one buffer, not a preference — `filetype` is redetected per
/// file, `endofline` is derived from the bytes a file was loaded with. Writing
/// `filetype = "rust"` into a global config would misapply it to every file
/// opened afterwards, so `:set filetype=…` stays session-scoped.
///
/// Values are seeded from the bundled `config.toml`, so this struct carries no
/// Rust-side defaults; `options_defaults_match_engine_defaults` pins the
/// bundled values against `hjkl_engine::types::Options::default()` so the two
/// sources of truth cannot drift.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsConfig {
    // ── indentation ──────────────────────────────────────────────────────
    pub shiftwidth: u32,
    pub tabstop: u32,
    pub softtabstop: u32,
    pub expandtab: bool,
    pub autoindent: bool,
    pub smartindent: bool,
    // ── search ───────────────────────────────────────────────────────────
    pub ignorecase: bool,
    pub smartcase: bool,
    pub wrapscan: bool,
    pub hlsearch: bool,
    pub incsearch: bool,
    // ── gutter and cursor highlights ─────────────────────────────────────
    pub number: bool,
    pub relativenumber: bool,
    pub numberwidth: usize,
    pub cursorline: bool,
    pub cursorcolumn: bool,
    /// `"yes"`, `"no"`, or `"auto"`.
    pub signcolumn: String,
    pub colorcolumn: String,
    // ── wrapping and scrolling ───────────────────────────────────────────
    /// `"off"`, `"char"`, or `"word"`. `:set wrap` selects `char` (or keeps
    /// `word` if `linebreak` already chose it); `:set linebreak` selects
    /// `word`. One tri-state key beats two bools that can contradict.
    pub wrap: String,
    pub textwidth: u32,
    pub scrolloff: usize,
    pub sidescrolloff: usize,
    pub scroll_duration_ms: u16,
    // ── folding ──────────────────────────────────────────────────────────
    /// `"manual"`, `"expr"`, or `"marker"` (`"syntax"` is accepted by `:set`
    /// as an alias for `"expr"` and normalises to it here).
    pub foldmethod: String,
    pub foldenable: bool,
    pub foldcolumn: u32,
    pub foldlevelstart: u32,
    pub foldmarker: String,
    // ── invisibles and guides ────────────────────────────────────────────
    pub list: bool,
    pub listchars: String,
    pub indent_guides: bool,
    pub indent_guide_char: char,
    // ── editing behaviour ────────────────────────────────────────────────
    pub iskeyword: String,
    pub formatoptions: String,
    pub autopair: bool,
    #[serde(rename = "autoclose-tag")]
    pub autoclose_tag: bool,
    pub motion_sneak: bool,
    pub matchparen: bool,
    pub undolevels: u32,
    pub undobreak: bool,
    /// Milliseconds; `:set timeoutlen`.
    pub timeoutlen: u64,
    pub updatetime: u32,
    // ── save behaviour ───────────────────────────────────────────────────
    pub format_on_save: bool,
    pub trim_trailing_whitespace: bool,
    pub fixendofline: bool,
    pub autoreload: bool,
    // ── external tooling ─────────────────────────────────────────────────
    pub makeprg: String,
    pub errorformat: String,
    // ── visual extras ────────────────────────────────────────────────────
    pub rainbow_brackets: bool,
    pub colorizer: bool,
    pub colorizer_filetypes: Vec<String>,
    pub tabline_icons: bool,
    pub blame_inline: bool,
    /// `"off"`, `"current"`, or `"all"`.
    pub diagnostics_inline: String,
    // ── modelines ────────────────────────────────────────────────────────
    pub modeline: bool,
    pub modelines: u32,
}

impl OptionsConfig {
    /// Overlay these values onto `o`.
    ///
    /// Only the fields [`hjkl_engine::types::Options`] actually carries are
    /// written here; the rest of the `:set` surface lives on `Settings` and is
    /// applied by [`Self::apply_to_settings`]. Splitting it this way keeps the
    /// precedence chain intact: the caller layers `.editorconfig` and then the
    /// file's modeline on top of the `Options` snapshot, and both only ever
    /// touch fields in this half.
    ///
    /// Unparseable enum strings fall back to the engine default rather than
    /// failing the load — [`Validate`] rejects them at startup with a pointer
    /// to the offending key, so reaching a fallback here means validation was
    /// bypassed (a programmatic caller), not that a user typo went unnoticed.
    pub fn apply_to_options(&self, o: &mut hjkl_engine::types::Options) {
        use hjkl_engine::types::{FoldMethod, SignColumnMode, WrapMode};

        o.shiftwidth = self.shiftwidth;
        o.tabstop = self.tabstop;
        o.softtabstop = self.softtabstop;
        o.expandtab = self.expandtab;
        o.autoindent = self.autoindent;
        o.smartindent = self.smartindent;

        o.ignorecase = self.ignorecase;
        o.smartcase = self.smartcase;
        o.wrapscan = self.wrapscan;
        o.hlsearch = self.hlsearch;
        o.incsearch = self.incsearch;

        o.number = self.number;
        o.relativenumber = self.relativenumber;
        o.numberwidth = self.numberwidth;
        o.cursorline = self.cursorline;
        o.cursorcolumn = self.cursorcolumn;
        o.signcolumn = match self.signcolumn.as_str() {
            "yes" => SignColumnMode::Yes,
            "no" => SignColumnMode::No,
            _ => SignColumnMode::Auto,
        };
        o.colorcolumn.clone_from(&self.colorcolumn);

        o.wrap = match self.wrap.as_str() {
            "char" => WrapMode::Char,
            "word" => WrapMode::Word,
            _ => WrapMode::None,
        };
        o.textwidth = self.textwidth;
        o.scrolloff = self.scrolloff;
        o.sidescrolloff = self.sidescrolloff;

        o.foldmethod = match self.foldmethod.as_str() {
            "manual" => FoldMethod::Manual,
            "marker" => FoldMethod::Marker,
            _ => FoldMethod::Expr,
        };
        o.foldenable = self.foldenable;
        o.foldcolumn = self.foldcolumn;
        o.foldlevelstart = self.foldlevelstart;
        o.foldmarker.clone_from(&self.foldmarker);

        o.list = self.list;
        if let Ok(lc) = hjkl_buffer::ListChars::parse(&self.listchars) {
            o.listchars = lc;
        }
        o.indent_guides = self.indent_guides;
        o.indent_guide_char = self.indent_guide_char;

        o.iskeyword.clone_from(&self.iskeyword);
        o.formatoptions.clone_from(&self.formatoptions);
        o.motion_sneak = self.motion_sneak;
        o.matchparen = self.matchparen;
        o.undo_levels = self.undolevels;
        o.undo_break_on_motion = self.undobreak;
        o.timeout_len = core::time::Duration::from_millis(self.timeoutlen);
        o.updatetime = self.updatetime;

        o.format_on_save = self.format_on_save;
        o.trim_trailing_whitespace = self.trim_trailing_whitespace;
        o.fixendofline = self.fixendofline;
        o.autoreload = self.autoreload;

        o.rainbow_brackets = self.rainbow_brackets;
        o.colorizer = self.colorizer;
        o.colorizer_filetypes.clone_from(&self.colorizer_filetypes);

        o.modeline = self.modeline;
        o.modelines = self.modelines;
    }

    /// Apply the `:set` options that have **no** [`hjkl_engine::types::Options`]
    /// counterpart directly onto `s`.
    ///
    /// Must run AFTER `Settings::apply_options`, which would otherwise
    /// overwrite these from a stale snapshot. Neither `.editorconfig` nor a
    /// modeline can express any of them, so applying them after the overlay
    /// costs no precedence.
    pub fn apply_to_settings(&self, s: &mut hjkl_engine::Settings) {
        use hjkl_engine::types::DiagInlineMode;

        s.makeprg.clone_from(&self.makeprg);
        s.errorformat.clone_from(&self.errorformat);
        s.autopair = self.autopair;
        s.autoclose_tag = self.autoclose_tag;
        s.tabline_icons = self.tabline_icons;
        s.blame_inline = self.blame_inline;
        s.scroll_duration_ms = self.scroll_duration_ms;
        s.diagnostics_inline = match self.diagnostics_inline.as_str() {
            "off" => DiagInlineMode::Off,
            "current" => DiagInlineMode::Current,
            _ => DiagInlineMode::All,
        };
    }
}

/// Strip a `:set` token down to the option name it addresses, or `None` when
/// the token must not trigger a write-back.
///
/// `None` for the query form (`name?`), which reads rather than writes.
/// `formatoptions+=`/`-=` resolve to plain `formatoptions`, whose *result* the
/// caller then persists by reading the post-set value.
///
/// The negation and toggle prefixes are recognised here because they name the
/// same option: `:set nolist`, `:set invlist` and `:set list!` all have to
/// write `options.list`. Resolution tries the WHOLE token first, so an option
/// whose name merely starts with `no` or `inv` is not mistaken for a prefixed
/// form — `number` is the live example.
pub fn setting_name_from_token(token: &str) -> Option<&str> {
    if token.ends_with('?') {
        return None;
    }
    if let Some(i) = token.find("+=").or_else(|| token.find("-=")) {
        return Some(&token[..i]);
    }
    let token = token.strip_suffix('!').unwrap_or(token);
    let name = token.split_once('=').map_or(token, |(n, _)| n);
    if options_key_for_setting(name).is_some() {
        return Some(name);
    }
    name.strip_prefix("no")
        .filter(|rest| options_key_for_setting(rest).is_some())
        .or_else(|| {
            name.strip_prefix("inv")
                .filter(|rest| options_key_for_setting(rest).is_some())
        })
}

/// Map a `:set` option name (canonical **or** alias) to its key inside the
/// `[options]` table, or `None` when the option must not be persisted.
///
/// Derived from [`hjkl_engine::options_registry`], so the config surface and
/// the `:set` surface cannot disagree about what exists or what it is called.
/// `None` means one of:
///
/// - **buffer-local** ([`OptScope::BufferLocal`]) — `filetype`,
///   `commentstring`, `readonly`, `modifiable`. Each describes one buffer, not
///   a preference; see [`OptionsConfig`].
/// - **host-owned** — `background`, `mouse`, `endofline`. Not engine settings;
///   the host applies them before `:set` reaches the engine, so they have no
///   registry entry.
///
/// `linebreak` is the one name that does not map to a key of its own: it is a
/// second spelling over the same tri-state as `wrap`, so both land on `"wrap"`.
pub fn options_key_for_setting(name: &str) -> Option<&'static str> {
    if name == "linebreak" || name == "lbr" {
        return Some("wrap");
    }
    let desc = hjkl_engine::options_registry::lookup(name)?;
    (desc.scope == hjkl_engine::options_registry::OptScope::Global).then_some(desc.name)
}

/// Write the live value of `set_name` into the `[options]` table of the config
/// file at `path`.
///
/// The value is read back off `settings` through the registry's own getter
/// rather than re-parsed from the `:set` token, so what lands in the file is
/// exactly what the editor ended up with — clamped, normalised, and with
/// `formatoptions+=` already folded in.
///
/// The TOML type follows [`OptKind`], so there is no per-option writer to keep
/// in step: booleans write booleans, `Int` writes an integer, `StrList` writes
/// an array, and everything else writes a string.
///
/// A name that is not persistable ([`options_key_for_setting`] returns `None`)
/// is a silent no-op: buffer-local options are session-scoped by design.
pub fn persist_option(
    path: &Path,
    set_name: &str,
    settings: &hjkl_engine::Settings,
) -> Result<(), ConfigError> {
    use hjkl_config::write_key_at;
    use hjkl_engine::options_registry::{OptKind, lookup};
    use hjkl_engine::types::OptionValue;

    let Some(key) = options_key_for_setting(set_name) else {
        return Ok(());
    };
    let dotted = format!("options.{key}");

    // `wrap` is the one key whose config spelling is richer than its `:set`
    // one: `:set wrap` / `:set linebreak` are two booleans over a tri-state,
    // and the file stores the tri-state so a reader can tell char-break from
    // word-break.
    if key == "wrap" {
        let mode = match settings.wrap {
            hjkl_buffer::Wrap::None => "off",
            hjkl_buffer::Wrap::Char => "char",
            hjkl_buffer::Wrap::Word => "word",
        };
        return write_key_at(path, &dotted, mode);
    }

    // Look up by the CANONICAL name — `key` is `desc.name` by construction.
    let desc = lookup(key).ok_or_else(|| ConfigError::Invalid {
        path: path.to_path_buf(),
        message: format!("`{key}` is persistable but has no registry entry"),
    })?;

    match ((desc.get)(settings), desc.kind) {
        (OptionValue::Bool(b), _) => write_key_at(path, &dotted, b),
        (OptionValue::Int(n), _) => write_key_at(path, &dotted, n),
        (OptionValue::String(s), OptKind::StrList) => {
            let arr: toml_edit::Array = s.split(',').filter(|p| !p.is_empty()).collect();
            write_key_at(path, &dotted, arr)
        }
        (OptionValue::String(s), _) => write_key_at(path, &dotted, s),
    }
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
    /// Theme name. Bundled schemes: `"tokyonight"` (default), `"dark"`,
    /// `"light"`. Unknown non-empty names fall back to the default with a
    /// startup warning.
    pub name: String,
    /// When `true`, the editor text-area + gutter are NOT painted with the
    /// theme background — the terminal's own background shows through (vim's
    /// transparent behaviour). The statusline/top-bar always paint their own
    /// backgrounds regardless. `#[serde(default)]` so configs predating this
    /// field keep parsing (defaults to `false`).
    #[serde(default)]
    pub transparent: bool,
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
        // Indentation widths. `shiftwidth`/`tabstop` must be non-zero — a zero
        // shift step is what `:set shiftwidth=0` already refuses at runtime, so
        // a config file must not be able to smuggle it in behind that check.
        ensure_range(self.options.shiftwidth, 1, 16, "options.shiftwidth")?;
        ensure_range(self.options.tabstop, 1, 16, "options.tabstop")?;
        ensure_range(self.options.softtabstop, 0, 16, "options.softtabstop")?;
        ensure_range(self.options.textwidth, 1, 10_000, "options.textwidth")?;
        // Same bounds `:set` enforces, so the two entry points agree.
        ensure_range(self.options.numberwidth, 1, 20, "options.numberwidth")?;
        ensure_range(self.options.foldcolumn, 0, 12, "options.foldcolumn")?;
        // Enum-valued keys: reject a typo at startup rather than silently
        // falling back to the default deep inside `apply_to_options`.
        ensure_one_of(
            &self.options.signcolumn.as_str(),
            &["yes", "no", "auto"],
            "options.signcolumn",
        )?;
        ensure_one_of(
            &self.options.wrap.as_str(),
            &["off", "char", "word"],
            "options.wrap",
        )?;
        ensure_one_of(
            &self.options.foldmethod.as_str(),
            &["manual", "expr", "syntax", "marker"],
            "options.foldmethod",
        )?;
        ensure_one_of(
            &self.options.diagnostics_inline.as_str(),
            &["off", "current", "all"],
            "options.diagnostics_inline",
        )?;
        // `listchars` has its own grammar; surface a parse failure as a
        // validation error naming the key instead of silently keeping the
        // default the first time the editor renders invisibles.
        if let Err(e) = hjkl_buffer::ListChars::parse(&self.options.listchars) {
            return Err(ValidationError::new("options.listchars", e));
        }
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
        assert_eq!(cfg.options.tabstop, 4);
        assert!(cfg.options.expandtab);
        assert!(cfg.editor.mouse, "mouse defaults on");
        assert_eq!(cfg.theme.name, "tokyonight");
        assert!(!cfg.theme.transparent, "transparent defaults off");
    }

    #[test]
    fn defaults_match_default_impl() {
        let parsed: Config = toml::from_str(DEFAULTS_TOML).unwrap();
        let dflt = Config::default();
        assert_eq!(parsed.editor.leader, dflt.editor.leader);
        assert_eq!(parsed.options.tabstop, dflt.options.tabstop);
        assert_eq!(parsed.theme.name, dflt.theme.name);
        assert_eq!(parsed.theme.transparent, dflt.theme.transparent);
    }

    #[test]
    fn user_partial_override_keeps_defaults() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "[editor]\nleader = \"\\\\\"").unwrap();
        let cfg = load_from(f.path()).unwrap();
        assert_eq!(cfg.editor.leader, '\\');
        assert_eq!(
            cfg.options.tabstop, 4,
            "non-overridden section keeps default"
        );
        assert_eq!(
            cfg.theme.name, "tokyonight",
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

    /// The bundled `[options]` table and the engine's own defaults are two
    /// spellings of the same thing, and nothing but this test keeps them
    /// saying it the same way.
    ///
    /// Applying the bundled table to a default `Options` must be a no-op. If
    /// it isn't, a fresh session silently disagrees with `Options::default()`
    /// — which is exactly the class of bug that made `cursorline` report one
    /// value and render another.
    #[test]
    fn options_defaults_match_engine_defaults() {
        let cfg = Config::default();
        let mut seeded = hjkl_engine::types::Options::default();
        cfg.options.apply_to_options(&mut seeded);
        assert_eq!(
            seeded,
            hjkl_engine::types::Options::default(),
            "bundled [options] drifted from Options::default()"
        );
    }

    /// Same contract for the half of the `:set` surface that lives on
    /// `Settings` rather than `Options`.
    #[test]
    fn options_defaults_match_engine_settings_defaults() {
        let cfg = Config::default();
        let mut seeded = hjkl_engine::Settings::default();
        cfg.options.apply_to_settings(&mut seeded);
        let dflt = hjkl_engine::Settings::default();
        assert_eq!(seeded.makeprg, dflt.makeprg);
        assert_eq!(seeded.errorformat, dflt.errorformat);
        assert_eq!(seeded.autopair, dflt.autopair);
        assert_eq!(seeded.autoclose_tag, dflt.autoclose_tag);
        assert_eq!(seeded.tabline_icons, dflt.tabline_icons);
        assert_eq!(seeded.blame_inline, dflt.blame_inline);
        assert_eq!(seeded.scroll_duration_ms, dflt.scroll_duration_ms);
        assert_eq!(seeded.diagnostics_inline, dflt.diagnostics_inline);
    }

    /// Every key `options_key_for_setting` can name must have a writer arm in
    /// `persist_option`.
    ///
    /// The fallback arm exists so a drift is an error rather than a silent
    /// no-op, but an error only surfaces when a user happens to `:set` that
    /// option. This drives every key through the writer so the drift is caught
    /// here instead.
    #[test]
    fn every_persistable_key_has_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let settings = hjkl_engine::Settings::default();

        let doc: toml::Value = toml::from_str(DEFAULTS_TOML).unwrap();
        for key in doc["options"].as_table().unwrap().keys() {
            // Reached via the key's own name, which is always a valid `:set`
            // name for a persistable option (that is the 1:1 rule).
            if options_key_for_setting(key).is_none() {
                continue; // config-only key — nothing to write.
            }
            persist_option(&path, key, &settings)
                .unwrap_or_else(|e| panic!("no writer for `options.{key}`: {e}"));
        }

        // And the file it produced must still load.
        let written = std::fs::read_to_string(&path).unwrap();
        if let Err(e) = toml::from_str::<Config>(&written) {
            // A partial file is expected (only [options] was written), so a
            // missing-section error is fine; a bad *key* is not.
            let msg = e.to_string();
            assert!(
                !msg.contains("unknown field"),
                "persist_option wrote a key the schema rejects: {msg}"
            );
        }
    }

    /// The buffer-local options must map to nothing, or `:set filetype=rust`
    /// would write a global default that misapplies to every later file.
    #[test]
    fn buffer_local_options_are_never_persistable() {
        for name in [
            "filetype",
            "ft",
            "commentstring",
            "cms",
            "readonly",
            "ro",
            "modifiable",
            "ma",
            "endofline",
            "eol",
        ] {
            assert!(
                options_key_for_setting(name).is_none(),
                "`:set {name}` is buffer-local and must not be persistable"
            );
        }
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
        cfg.options.tabstop = 0;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "options.tabstop");
    }

    #[test]
    fn validate_rejects_huge_tab_width() {
        let mut cfg = Config::default();
        cfg.options.tabstop = 64;
        let err = cfg.validate().unwrap_err();
        assert_eq!(err.field, "options.tabstop");
        assert!(err.message.contains("64"));
    }

    #[test]
    fn validate_accepts_tab_width_boundary() {
        let mut cfg = Config::default();
        cfg.options.tabstop = 1;
        assert!(cfg.validate().is_ok());
        cfg.options.tabstop = 16;
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
        assert_eq!(cfg.options.tabstop, 4, "tabstop must keep default");
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
