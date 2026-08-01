//! One table describing every `:set`-able option.
//!
//! # Why this exists
//!
//! An option's name used to appear in roughly twenty hand-maintained places —
//! seven tables in `hjkl_ex::setopt` alone (the completion name list, the
//! boolean list, the enum-value map, the `?` query match, the `name=value`
//! match, the boolean match, and the `!` toggle match), plus the `Settings` and
//! `Options` structs, their defaults and converters, and the host's config-key
//! mapping and TOML writer.
//!
//! Nothing kept them in step, and they drifted. Found in one audit:
//!
//! - `:set inv<name>` was offered by completion and rejected by the parser, for
//!   every boolean option — the `inv` prefix had no arm at all.
//! - `:set <name>!` was implemented for four options and errored for the other
//!   thirty.
//! - `:set modifiable?` / `linebreak?` / `motion_sneak?` had no query arm.
//! - `updatetime`, `colorizer` and `colorizer_filetypes` had `Settings` fields
//!   and no `:set` name whatsoever.
//!
//! Every one of those is a table that was updated while its siblings were not.
//! [`OPTIONS`] replaces the lot: a name, its aliases, its type, whether it is
//! global or buffer-local, and a getter/setter pair against [`Settings`]. The
//! `no…` / `inv…` / `!` / `?` forms are then properties of
//! [`OptKind::Bool`] rather than four independent lists, so they cannot
//! disagree.
//!
//! # What is deliberately NOT here
//!
//! - **`Options` / `OptionsConfig` struct fields.** Both need compile-time
//!   fields (one is the engine's snapshot type, the other is a `serde`
//!   schema), so they stay hand-written. Tests pin them against this table
//!   rather than a macro generating them.
//! - **Host-owned pseudo-options** — `mouse`, `background`, `endofline`. None
//!   is a [`Settings`] field; the host intercepts them before the engine is
//!   consulted.

use crate::editor::Settings;
use crate::types::{DiagInlineMode, FoldMethod, OptionValue, SignColumnMode};

/// What kind of value an option holds, and what `:set` will accept for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptKind {
    /// `:set name`, `:set noname`, `:set invname`, `:set name!`,
    /// `:set name=on|off|true|false|yes|no|1|0`.
    Bool,
    /// `:set name=N`, rejected outside `min..=max`.
    Int { min: u32, max: u32 },
    /// `:set name=text`.
    Str,
    /// `:set name=one-of`. The slice is also the completion candidate list.
    Enum(&'static [&'static str]),
    /// A comma-separated list. Transported as [`OptionValue::String`]; hosts
    /// that have a richer representation (TOML arrays) split on `,`.
    StrList,
}

/// Whether an option describes the session or a single buffer.
///
/// [`OptScope::BufferLocal`] options are never written to a config file:
/// `filetype` is redetected per file and `readonly` is a property of the
/// buffer at hand, so persisting either would misapply it to every file opened
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptScope {
    Global,
    BufferLocal,
}

/// One `:set`-able option.
pub struct OptionDesc {
    /// Canonical name, as `:set name?` echoes it and as a config key.
    pub name: &'static str,
    /// Vim-style short forms (`cul` for `cursorline`).
    pub aliases: &'static [&'static str],
    pub kind: OptKind,
    pub scope: OptScope,
    /// Read the live value.
    pub get: fn(&Settings) -> OptionValue,
    /// Write it, rejecting a value the option cannot hold.
    pub set: fn(&mut Settings, OptionValue) -> Result<(), String>,
}

impl OptionDesc {
    /// True when `name` is this option's canonical name or one of its aliases.
    pub fn matches(&self, name: &str) -> bool {
        self.name == name || self.aliases.contains(&name)
    }

    /// Canonical name plus every alias, for completion candidate lists.
    pub fn all_names(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }
}

/// Find an option by canonical name or alias.
pub fn lookup(name: &str) -> Option<&'static OptionDesc> {
    OPTIONS.iter().find(|d| d.matches(name))
}

// ── value coercion helpers ───────────────────────────────────────────────────

fn want_bool(name: &str, v: OptionValue) -> Result<bool, String> {
    match v {
        OptionValue::Bool(b) => Ok(b),
        // `:set name=1` / `=0`, and the nvim API's integer booleans.
        OptionValue::Int(n) => Ok(n != 0),
        other => Err(format!("option `{name}` expects a boolean, got {other:?}")),
    }
}

fn want_int(name: &str, v: OptionValue) -> Result<i64, String> {
    match v {
        OptionValue::Int(n) => Ok(n),
        other => Err(format!("option `{name}` expects a number, got {other:?}")),
    }
}

fn want_str(name: &str, v: OptionValue) -> Result<String, String> {
    match v {
        OptionValue::String(s) => Ok(s),
        other => Err(format!("option `{name}` expects a string, got {other:?}")),
    }
}

// ── entry constructors ───────────────────────────────────────────────────────
//
// Each expands to the `(get, set)` pair for one field shape. Non-capturing
// closures coerce to `fn` pointers, so the table stays a `const`.

macro_rules! opt_bool {
    ($name:literal, $aliases:expr, $scope:ident, $field:ident) => {
        OptionDesc {
            name: $name,
            aliases: $aliases,
            kind: OptKind::Bool,
            scope: OptScope::$scope,
            get: |s: &Settings| OptionValue::Bool(s.$field),
            set: |s: &mut Settings, v: OptionValue| {
                s.$field = want_bool($name, v)?;
                Ok(())
            },
        }
    };
}

macro_rules! opt_int {
    ($name:literal, $aliases:expr, $scope:ident, $field:ident, $ty:ty, $min:literal, $max:expr) => {
        OptionDesc {
            name: $name,
            aliases: $aliases,
            kind: OptKind::Int {
                min: $min,
                max: $max,
            },
            scope: OptScope::$scope,
            get: |s: &Settings| OptionValue::Int(s.$field as i64),
            set: |s: &mut Settings, v: OptionValue| {
                let n = want_int($name, v)?;
                if n < i64::from($min) || n > i64::from($max) {
                    return Err(format!(
                        "{} must be in range {}..={}, got {n}",
                        $name, $min, $max
                    ));
                }
                s.$field = n as $ty;
                Ok(())
            },
        }
    };
}

macro_rules! opt_str {
    ($name:literal, $aliases:expr, $scope:ident, $field:ident) => {
        OptionDesc {
            name: $name,
            aliases: $aliases,
            kind: OptKind::Str,
            scope: OptScope::$scope,
            get: |s: &Settings| OptionValue::String(s.$field.clone()),
            set: |s: &mut Settings, v: OptionValue| {
                s.$field = want_str($name, v)?;
                Ok(())
            },
        }
    };
}

/// `min`/`max` are `u32` in [`OptKind::Int`]; `u32::MAX` is the "unbounded"
/// spelling.
const UNBOUNDED: u32 = u32::MAX;

const SIGNCOLUMN_VALUES: &[&str] = &["yes", "no", "auto"];
const FOLDMETHOD_VALUES: &[&str] = &["manual", "expr", "syntax", "marker"];
const DIAGINLINE_VALUES: &[&str] = &["off", "current", "all"];

/// Every `:set`-able option, in `:set all` display order (grouped by concern).
pub const OPTIONS: &[OptionDesc] = &[
    // ── indentation ─────────────────────────────────────────────────────────
    opt_int!(
        "shiftwidth",
        &["sw"],
        Global,
        shiftwidth,
        usize,
        1,
        UNBOUNDED
    ),
    opt_int!("tabstop", &["ts"], Global, tabstop, usize, 1, UNBOUNDED),
    opt_int!(
        "softtabstop",
        &["sts"],
        Global,
        softtabstop,
        usize,
        0,
        UNBOUNDED
    ),
    opt_bool!("expandtab", &["et"], Global, expandtab),
    opt_bool!("autoindent", &["ai"], Global, autoindent),
    opt_bool!("smartindent", &["si"], Global, smartindent),
    // ── search ──────────────────────────────────────────────────────────────
    opt_bool!("ignorecase", &["ic"], Global, ignore_case),
    opt_bool!("smartcase", &["scs"], Global, smartcase),
    opt_bool!("wrapscan", &["ws"], Global, wrapscan),
    opt_bool!("hlsearch", &["hls"], Global, hlsearch),
    opt_bool!("incsearch", &["is"], Global, incsearch),
    // ── gutter and cursor highlights ────────────────────────────────────────
    opt_bool!("number", &["nu"], Global, number),
    opt_bool!("relativenumber", &["rnu"], Global, relativenumber),
    opt_int!("numberwidth", &["nuw"], Global, numberwidth, usize, 1, 20),
    opt_bool!("cursorline", &["cul"], Global, cursorline),
    opt_bool!("cursorcolumn", &["cuc"], Global, cursorcolumn),
    OptionDesc {
        name: "signcolumn",
        aliases: &["scl"],
        kind: OptKind::Enum(SIGNCOLUMN_VALUES),
        scope: OptScope::Global,
        get: |s| {
            OptionValue::String(
                match s.signcolumn {
                    SignColumnMode::Yes => "yes",
                    SignColumnMode::No => "no",
                    SignColumnMode::Auto => "auto",
                }
                .to_string(),
            )
        },
        set: |s, v| {
            s.signcolumn = match want_str("signcolumn", v)?.as_str() {
                "yes" => SignColumnMode::Yes,
                "no" => SignColumnMode::No,
                "auto" => SignColumnMode::Auto,
                other => {
                    return Err(format!(
                        "signcolumn must be `yes`, `no`, or `auto`, got `{other}`"
                    ));
                }
            };
            Ok(())
        },
    },
    opt_str!("colorcolumn", &["cc"], Global, colorcolumn),
    // ── wrapping and scrolling ──────────────────────────────────────────────
    //
    // `wrap` and `linebreak` are two `:set` names over one tri-state field.
    // `:set wrap` picks char-break unless `linebreak` already chose word-break;
    // `:set nolinebreak` drops back to char-break rather than turning wrapping
    // off. Keeping both here (rather than one "wrap mode" enum) is what makes
    // `:set nowrap` and `:set invlinebreak` behave the way vim's do.
    OptionDesc {
        name: "wrap",
        aliases: &[],
        kind: OptKind::Bool,
        scope: OptScope::Global,
        get: |s| OptionValue::Bool(s.wrap != hjkl_buffer::Wrap::None),
        set: |s, v| {
            s.wrap = if want_bool("wrap", v)? {
                match s.wrap {
                    hjkl_buffer::Wrap::Word => hjkl_buffer::Wrap::Word,
                    _ => hjkl_buffer::Wrap::Char,
                }
            } else {
                hjkl_buffer::Wrap::None
            };
            Ok(())
        },
    },
    OptionDesc {
        name: "linebreak",
        aliases: &["lbr"],
        kind: OptKind::Bool,
        scope: OptScope::Global,
        get: |s| OptionValue::Bool(s.wrap == hjkl_buffer::Wrap::Word),
        set: |s, v| {
            s.wrap = if want_bool("linebreak", v)? {
                hjkl_buffer::Wrap::Word
            } else {
                match s.wrap {
                    hjkl_buffer::Wrap::None => hjkl_buffer::Wrap::None,
                    _ => hjkl_buffer::Wrap::Char,
                }
            };
            Ok(())
        },
    },
    opt_int!("textwidth", &["tw"], Global, textwidth, usize, 1, UNBOUNDED),
    opt_int!("scrolloff", &["so"], Global, scrolloff, usize, 0, UNBOUNDED),
    opt_int!(
        "sidescrolloff",
        &["siso"],
        Global,
        sidescrolloff,
        usize,
        0,
        UNBOUNDED
    ),
    opt_int!(
        "scroll_duration_ms",
        &[],
        Global,
        scroll_duration_ms,
        u16,
        0,
        65535
    ),
    // ── folding ─────────────────────────────────────────────────────────────
    OptionDesc {
        name: "foldmethod",
        aliases: &["fdm"],
        kind: OptKind::Enum(FOLDMETHOD_VALUES),
        scope: OptScope::Global,
        get: |s| {
            OptionValue::String(
                match s.foldmethod {
                    FoldMethod::Manual => "manual",
                    FoldMethod::Expr => "expr",
                    FoldMethod::Marker => "marker",
                }
                .to_string(),
            )
        },
        set: |s, v| {
            s.foldmethod = match want_str("foldmethod", v)?.as_str() {
                "manual" => FoldMethod::Manual,
                // vim spells tree-sitter-driven folding `syntax`; hjkl calls
                // the same thing `expr`. Accepted, normalised on read-back.
                "expr" | "syntax" => FoldMethod::Expr,
                "marker" => FoldMethod::Marker,
                other => {
                    return Err(format!(
                        "foldmethod must be `manual`, `expr`, `syntax`, or `marker`, got `{other}`"
                    ));
                }
            };
            Ok(())
        },
    },
    opt_bool!("foldenable", &["fen"], Global, foldenable),
    opt_int!("foldcolumn", &["fdc"], Global, foldcolumn, u32, 0, 12),
    opt_int!(
        "foldlevelstart",
        &["fls"],
        Global,
        foldlevelstart,
        u32,
        0,
        UNBOUNDED
    ),
    opt_str!("foldmarker", &["fmr"], Global, foldmarker),
    // ── invisibles and guides ───────────────────────────────────────────────
    opt_bool!("list", &[], Global, list),
    OptionDesc {
        name: "listchars",
        aliases: &["lcs"],
        kind: OptKind::Str,
        scope: OptScope::Global,
        get: |s| OptionValue::String(s.listchars.to_canonical_string()),
        set: |s, v| {
            s.listchars = hjkl_buffer::ListChars::parse(&want_str("listchars", v)?)?;
            Ok(())
        },
    },
    opt_bool!("indent_guides", &["ig"], Global, indent_guides),
    OptionDesc {
        name: "indent_guide_char",
        aliases: &["igc"],
        kind: OptKind::Str,
        scope: OptScope::Global,
        get: |s| OptionValue::String(s.indent_guide_char.to_string()),
        set: |s, v| {
            let text = want_str("indent_guide_char", v)?;
            let mut chars = text.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                return Err(format!(
                    "indent_guide_char expects exactly one character, got {text:?}"
                ));
            };
            s.indent_guide_char = ch;
            Ok(())
        },
    },
    // ── editing behaviour ───────────────────────────────────────────────────
    opt_str!("iskeyword", &["isk"], Global, iskeyword),
    opt_str!("formatoptions", &["fo"], Global, formatoptions),
    opt_bool!("autopair", &["ap"], Global, autopair),
    opt_bool!("autoclose-tag", &["act"], Global, autoclose_tag),
    opt_bool!("motion_sneak", &["snk"], Global, motion_sneak),
    opt_bool!("matchparen", &["mps"], Global, matchparen),
    opt_int!(
        "undolevels",
        &["ul"],
        Global,
        undo_levels,
        u32,
        0,
        UNBOUNDED
    ),
    opt_bool!("undobreak", &[], Global, undo_break_on_motion),
    OptionDesc {
        name: "timeoutlen",
        aliases: &["tm"],
        kind: OptKind::Int {
            min: 0,
            max: UNBOUNDED,
        },
        scope: OptScope::Global,
        get: |s| OptionValue::Int(s.timeout_len.as_millis() as i64),
        set: |s, v| {
            let n = want_int("timeoutlen", v)?;
            if n < 0 {
                return Err(format!("timeoutlen must not be negative, got {n}"));
            }
            s.timeout_len = core::time::Duration::from_millis(n as u64);
            Ok(())
        },
    },
    opt_int!("updatetime", &["ut"], Global, updatetime, u32, 0, UNBOUNDED),
    // ── save behaviour ──────────────────────────────────────────────────────
    opt_bool!("format_on_save", &["fos"], Global, format_on_save),
    opt_bool!(
        "trim_trailing_whitespace",
        &["tts"],
        Global,
        trim_trailing_whitespace
    ),
    opt_bool!("fixendofline", &["fixeol"], Global, fixendofline),
    opt_bool!("autoreload", &["ar"], Global, autoreload),
    // ── external tooling ────────────────────────────────────────────────────
    opt_str!("makeprg", &["mp"], Global, makeprg),
    opt_str!("errorformat", &["efm"], Global, errorformat),
    // ── visual extras ───────────────────────────────────────────────────────
    opt_bool!("rainbow_brackets", &["rb"], Global, rainbow_brackets),
    opt_bool!("colorizer", &[], Global, colorizer),
    OptionDesc {
        name: "colorizer_filetypes",
        aliases: &[],
        kind: OptKind::StrList,
        scope: OptScope::Global,
        get: |s| OptionValue::String(s.colorizer_filetypes.join(",")),
        set: |s, v| {
            // Empty entries are dropped so a trailing comma is harmless rather
            // than creating a filetype named "" that matches nothing.
            s.colorizer_filetypes = want_str("colorizer_filetypes", v)?
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect();
            Ok(())
        },
    },
    opt_bool!("tabline_icons", &[], Global, tabline_icons),
    opt_bool!("blame_inline", &[], Global, blame_inline),
    OptionDesc {
        name: "diagnostics_inline",
        aliases: &["diaginline"],
        kind: OptKind::Enum(DIAGINLINE_VALUES),
        scope: OptScope::Global,
        get: |s| {
            OptionValue::String(
                match s.diagnostics_inline {
                    DiagInlineMode::Off => "off",
                    DiagInlineMode::Current => "current",
                    DiagInlineMode::All => "all",
                }
                .to_string(),
            )
        },
        set: |s, v| {
            s.diagnostics_inline = match want_str("diagnostics_inline", v)?.as_str() {
                "off" | "no" | "disable" | "disabled" => DiagInlineMode::Off,
                "current" | "cursor" | "line" => DiagInlineMode::Current,
                "all" | "on" | "enable" | "enabled" => DiagInlineMode::All,
                other => {
                    return Err(format!(
                        "diagnostics_inline must be `off`, `current`, or `all`, got `{other}`"
                    ));
                }
            };
            Ok(())
        },
    },
    // ── modelines ───────────────────────────────────────────────────────────
    opt_bool!("modeline", &["ml"], Global, modeline),
    opt_int!("modelines", &["mls"], Global, modelines, u32, 0, UNBOUNDED),
    // ── buffer-local (never persisted) ──────────────────────────────────────
    opt_bool!("readonly", &["ro"], BufferLocal, readonly),
    opt_bool!("modifiable", &["ma"], BufferLocal, modifiable),
    opt_str!("filetype", &["ft"], BufferLocal, filetype),
    opt_str!("commentstring", &["cms"], BufferLocal, commentstring),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in OPTIONS {
            for n in d.all_names() {
                assert!(seen.insert(n), "`{n}` is registered twice");
            }
        }
    }

    /// Every entry must round-trip its own default: `set(get(x))` is a no-op.
    /// Catches a getter and setter that disagree about representation — the
    /// enum spellings and the `wrap` / `linebreak` pair especially.
    #[test]
    fn get_set_round_trips_for_every_option() {
        for d in OPTIONS {
            let mut s = Settings::default();
            let before = (d.get)(&s);
            (d.set)(&mut s, before.clone())
                .unwrap_or_else(|e| panic!("`{}` rejected its own value: {e}", d.name));
            let after = (d.get)(&s);
            assert_eq!(before, after, "`{}` did not round-trip", d.name);
        }
    }

    /// A boolean must actually flip, or the `no…` / `inv…` / `!` forms built
    /// on top of `OptKind::Bool` would silently do nothing.
    #[test]
    fn every_bool_option_flips() {
        for d in OPTIONS.iter().filter(|d| d.kind == OptKind::Bool) {
            let mut s = Settings::default();
            let OptionValue::Bool(before) = (d.get)(&s) else {
                panic!("`{}` is OptKind::Bool but does not get a Bool", d.name);
            };
            (d.set)(&mut s, OptionValue::Bool(!before)).unwrap();
            let OptionValue::Bool(after) = (d.get)(&s) else {
                unreachable!()
            };
            assert_eq!(after, !before, "`{}` did not flip", d.name);
        }
    }

    /// Enum options must accept every value they advertise for completion.
    #[test]
    fn every_enum_value_is_accepted() {
        for d in OPTIONS {
            let OptKind::Enum(values) = d.kind else {
                continue;
            };
            for v in values {
                let mut s = Settings::default();
                (d.set)(&mut s, OptionValue::String((*v).to_string())).unwrap_or_else(|e| {
                    panic!(
                        "`{}` advertises `{v}` for completion but rejects it: {e}",
                        d.name
                    )
                });
            }
        }
    }

    /// Integer bounds must admit their own endpoints, and reject just past
    /// them.
    #[test]
    fn int_bounds_admit_endpoints_and_reject_beyond() {
        for d in OPTIONS {
            let OptKind::Int { min, max } = d.kind else {
                continue;
            };
            let mut s = Settings::default();
            (d.set)(&mut s, OptionValue::Int(i64::from(min)))
                .unwrap_or_else(|e| panic!("`{}` rejected its own min {min}: {e}", d.name));
            if min > 0 {
                assert!(
                    (d.set)(&mut s, OptionValue::Int(i64::from(min) - 1)).is_err(),
                    "`{}` accepted a value below min {min}",
                    d.name
                );
            }
            if max != UNBOUNDED {
                (d.set)(&mut s, OptionValue::Int(i64::from(max)))
                    .unwrap_or_else(|e| panic!("`{}` rejected its own max {max}: {e}", d.name));
                assert!(
                    (d.set)(&mut s, OptionValue::Int(i64::from(max) + 1)).is_err(),
                    "`{}` accepted a value above max {max}",
                    d.name
                );
            }
        }
    }

    #[test]
    fn lookup_finds_by_alias() {
        assert_eq!(lookup("cul").unwrap().name, "cursorline");
        assert_eq!(lookup("ts").unwrap().name, "tabstop");
        assert!(lookup("definitely-not-an-option").is_none());
    }

    /// The buffer-local set is a deliberate, short list — anything landing in
    /// it by accident would stop being persistable without anyone noticing.
    #[test]
    fn buffer_local_set_is_exactly_the_documented_four() {
        let mut got: Vec<&str> = OPTIONS
            .iter()
            .filter(|d| d.scope == OptScope::BufferLocal)
            .map(|d| d.name)
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            ["commentstring", "filetype", "modifiable", "readonly"],
            "the buffer-local set changed — is the new option really per-buffer?"
        );
    }
}
