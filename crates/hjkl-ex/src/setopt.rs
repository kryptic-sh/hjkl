//! `:set [option ...]` command.
//!
//! Every form — `name`, `noname`, `invname`, `name!`, `name?`, `name=value` —
//! resolves through [`hjkl_engine::options_registry`], so the set of options
//! each accepts is one table rather than seven that could disagree. This module
//! owns only the *syntax*: which suffix means what, and how a value is spelled
//! on the command line.

use crate::effect::ExEffect;
use hjkl_engine::Host;
use hjkl_engine::options_registry::{OPTIONS, OptKind, OptionDesc, lookup};
use hjkl_engine::types::OptionValue;

/// Names the host intercepts before `:set` reaches the engine.
///
/// None is a `Settings` field, so none is in the registry: `background` reloads
/// the theme, `mouse` drives terminal capture, and `endofline` is buffer-local
/// state derived from the bytes a file was loaded with. They are listed here so
/// completion still offers them and so a token that slips through to the engine
/// is accepted rather than reported as unknown.
pub const HOST_OWNED_NAMES: &[&str] = &["background", "bg", "mouse", "endofline", "eol"];

/// Host-owned names that behave like booleans for `:set no…` / `inv…`
/// completion. `background` is an enum, so it is excluded.
const HOST_OWNED_BOOLS: &[&str] = &["mouse", "endofline"];

/// Every `:set` option name and short alias, for the `Setting` arg completer.
///
/// Derived from [`OPTIONS`] plus [`HOST_OWNED_NAMES`] — the hand-maintained
/// list this replaces had drifted, offering `inv…` forms the parser rejected
/// and omitting `modifiable`, `motion_sneak`, `updatetime` and `colorizer`
/// entirely.
pub fn all_setting_names() -> Vec<String> {
    OPTIONS
        .iter()
        .flat_map(OptionDesc::all_names)
        .chain(HOST_OWNED_NAMES.iter().copied())
        .map(str::to_string)
        .collect()
}

/// Canonical names of the boolean options, for the `:set no…` / `inv…`
/// completion arm. Names only (no aliases) — vim lists the long forms.
pub fn boolean_setting_names() -> Vec<String> {
    OPTIONS
        .iter()
        .filter(|d| d.kind == OptKind::Bool)
        .map(|d| d.name)
        .chain(HOST_OWNED_BOOLS.iter().copied())
        .map(str::to_string)
        .collect()
}

/// Completion values for an enum-valued option, keyed by canonical name or
/// alias. Empty for every other kind and for unknown names.
///
/// Comes straight off [`OptKind::Enum`], which the setter also validates
/// against, so a candidate can never be offered and then rejected.
pub fn setting_value_candidates(name: &str) -> Vec<&'static str> {
    if matches!(name, "background" | "bg") {
        // Host-owned; the theme reload happens in the app.
        return vec!["dark", "light"];
    }
    match lookup(name).map(|d| d.kind) {
        Some(OptKind::Enum(values)) => {
            let mut v = values.to_vec();
            v.sort_unstable();
            v
        }
        _ => Vec::new(),
    }
}

pub fn apply_set<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    body: &str,
) -> ExEffect {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        let s = editor.settings();
        let wrap = match s.wrap {
            hjkl_buffer::Wrap::None => "off",
            hjkl_buffer::Wrap::Char => "char",
            hjkl_buffer::Wrap::Word => "word",
        };
        let scl = match s.signcolumn {
            hjkl_engine::types::SignColumnMode::Yes => "yes",
            hjkl_engine::types::SignColumnMode::No => "no",
            hjkl_engine::types::SignColumnMode::Auto => "auto",
        };
        let fdm = match s.foldmethod {
            hjkl_engine::types::FoldMethod::Manual => "manual",
            hjkl_engine::types::FoldMethod::Expr => "expr",
            hjkl_engine::types::FoldMethod::Marker => "marker",
        };
        return ExEffect::Info(format!(
            "shiftwidth={}  tabstop={}  softtabstop={}  textwidth={}  undolevels={}  timeoutlen={}  iskeyword=\"{}\"  expandtab={}  ignorecase={}  smartcase={}  wrapscan={}  autoindent={}  smartindent={}  undobreak={}  readonly={}  wrap={}  number={}  relativenumber={}  numberwidth={}  cursorline={}  cursorcolumn={}  signcolumn={}  foldcolumn={}  foldmethod={}  foldenable={}  foldlevelstart={}  colorcolumn=\"{}\"  formatoptions=\"{}\"  filetype=\"{}\"  commentstring=\"{}\"  makeprg=\"{}\"  errorformat=\"{}\"  autopair={}  autoclose-tag={}  scrolloff={}  sidescrolloff={}  list={}  listchars=\"{}\"  indent_guides={}  indent_guide_char={}  format_on_save={}  trim_trailing_whitespace={}  rainbow_brackets={}  matchparen={}  autoreload={}  scroll_duration_ms={}  tabline_icons={}",
            s.shiftwidth,
            s.tabstop,
            s.softtabstop,
            s.textwidth,
            s.undo_levels,
            s.timeout_len.as_millis(),
            s.iskeyword,
            if s.expandtab { "on" } else { "off" },
            if s.ignore_case { "on" } else { "off" },
            if s.smartcase { "on" } else { "off" },
            if s.wrapscan { "on" } else { "off" },
            if s.autoindent { "on" } else { "off" },
            if s.smartindent { "on" } else { "off" },
            if s.undo_break_on_motion { "on" } else { "off" },
            if s.readonly { "on" } else { "off" },
            wrap,
            if s.number { "on" } else { "off" },
            if s.relativenumber { "on" } else { "off" },
            s.numberwidth,
            if s.cursorline { "on" } else { "off" },
            if s.cursorcolumn { "on" } else { "off" },
            scl,
            s.foldcolumn,
            fdm,
            if s.foldenable { "on" } else { "off" },
            s.foldlevelstart,
            s.colorcolumn,
            s.formatoptions,
            s.filetype,
            s.commentstring,
            s.makeprg,
            s.errorformat,
            if s.autopair { "on" } else { "off" },
            if s.autoclose_tag { "on" } else { "off" },
            s.scrolloff,
            s.sidescrolloff,
            if s.list { "on" } else { "off" },
            s.listchars.to_canonical_string(),
            if s.indent_guides { "on" } else { "off" },
            s.indent_guide_char,
            if s.format_on_save { "on" } else { "off" },
            if s.trim_trailing_whitespace {
                "on"
            } else {
                "off"
            },
            if s.rainbow_brackets { "on" } else { "off" },
            if s.matchparen { "on" } else { "off" },
            if s.autoreload { "on" } else { "off" },
            s.scroll_duration_ms,
            if s.tabline_icons { "on" } else { "off" },
        ));
    }
    let mut query_lines: Vec<String> = Vec::new();
    for token in trimmed.split_whitespace() {
        // `:set <name>?` — print current value instead of mutating.
        // Vim convention; works for any option known to query_option_value.
        if let Some(name) = token.strip_suffix('?') {
            match query_option_value(editor, name) {
                Some(v) => query_lines.push(format!("{name}={v}")),
                None => return ExEffect::Error(format!("unknown :set option `{name}`")),
            }
            continue;
        }
        if let Err(e) = apply_set_token(editor, token) {
            return ExEffect::Error(e);
        }
    }
    if !query_lines.is_empty() {
        return ExEffect::Info(query_lines.join("  "));
    }
    ExEffect::Ok
}

/// Return a display-form string for `name`'s current value, or `None`
/// when the option isn't recognised. Used by the `:set <name>?` query
/// form and by the nvim-api `nvim_get_option_value` handler. Names
/// accepted match those in [`apply_set_token`] (both the long and short
/// aliases).
/// Display-form value for `:set name?`, or `None` when `name` is not a known
/// option. Also serves the nvim-api `nvim_get_option_value` handler.
///
/// String-valued options are quoted, matching vim's `:set` echo.
pub fn query_option_value<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::View, H>,
    name: &str,
) -> Option<String> {
    let desc = lookup(name)?;
    Some(match (desc.get)(editor.settings()) {
        OptionValue::Bool(b) => if b { "on" } else { "off" }.to_string(),
        OptionValue::Int(n) => n.to_string(),
        OptionValue::String(s) => match desc.kind {
            OptKind::Str | OptKind::StrList => format!("\"{s}\""),
            _ => s,
        },
    })
}

/// Parse a vim-ish boolean keyword used on the right of `name=value` for a
/// boolean option (`:set format_on_save=off`). Returns `None` for anything
/// that isn't a boolean literal so numeric options fall through unaffected.
fn parse_bool_word(s: &str) -> Option<bool> {
    match s {
        "on" | "true" | "yes" | "1" => Some(true),
        "off" | "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

/// Apply a single `:set` token. Supports `name=value`, `name+=flags`,
/// `name-=flags`, bare `name` (turns booleans on), `noname`, `invname`, and
/// `name!`.
///
/// Every form resolves through [`lookup`], so the set of options each one
/// accepts is the registry — not a per-form list. The `!` and `inv` forms used
/// to have their own match arms: `!` named four options and errored on the
/// other thirty, and `inv` had no arm at all despite completion offering it.
///
/// Exposed as `pub` so the nvim-api layer can call it directly with a
/// pre-constructed token (e.g. `makeprg=cargo build`) without going through
/// `apply_set`'s whitespace-splitting loop.
pub fn apply_set_token<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    token: &str,
) -> Result<(), String> {
    // `formatoptions+=flags` / `-=flags` — the one option with accumulating
    // syntax, so it stays a special case ahead of the table.
    for (suffix, add) in [("+=", true), ("-=", false)] {
        let stripped = token
            .strip_prefix(&format!("formatoptions{suffix}"))
            .or_else(|| token.strip_prefix(&format!("fo{suffix}")));
        if let Some(rest) = stripped {
            let mut fo = editor.settings().formatoptions.clone();
            for ch in rest.chars() {
                if add {
                    if !fo.contains(ch) {
                        fo.push(ch);
                    }
                } else {
                    fo = fo.chars().filter(|&c| c != ch).collect();
                }
            }
            editor.settings_mut().formatoptions = fo;
            return Ok(());
        }
    }

    if let Some((name, value)) = token.split_once('=') {
        if HOST_OWNED_NAMES.contains(&name) {
            // Applied by the host before we are reached; accept silently.
            return Ok(());
        }
        let desc = lookup(name).ok_or_else(|| format!("unknown :set option `{name}`"))?;
        let parsed = match desc.kind {
            OptKind::Bool => OptionValue::Bool(
                parse_bool_word(value)
                    .ok_or_else(|| format!("bad value `{value}` for :set {name}"))?,
            ),
            OptKind::Int { .. } => OptionValue::Int(
                value
                    .parse::<i64>()
                    .map_err(|_| format!("bad value `{value}` for :set {name}"))?,
            ),
            OptKind::Str | OptKind::StrList | OptKind::Enum(_) => {
                OptionValue::String(value.to_string())
            }
        };
        return (desc.set)(editor.settings_mut(), parsed);
    }

    // `name!` and `invname` both flip a boolean; `noname` clears it; a bare
    // `name` sets it.
    let (name, action) = if let Some(rest) = token.strip_suffix('!') {
        (rest, None)
    } else if let Some(rest) = token.strip_prefix("inv") {
        // Only treat `inv` as a prefix when what follows is a real boolean —
        // otherwise an option whose own name starts with `inv` would be
        // unreachable, the same trap `no` sets for `number`.
        match lookup(rest) {
            Some(d) if d.kind == OptKind::Bool => (rest, None),
            _ => (token, Some(true)),
        }
    } else if let Some(rest) = token.strip_prefix("no") {
        // `number` starts with `no`; try the whole name first.
        match lookup(token) {
            Some(_) => (token, Some(true)),
            None => (rest, Some(false)),
        }
    } else {
        (token, Some(true))
    };

    if HOST_OWNED_NAMES.contains(&name) {
        return Ok(());
    }
    let desc = lookup(name).ok_or_else(|| format!("unknown :set option `{name}`"))?;
    if desc.kind != OptKind::Bool {
        return Err(format!(
            "option `{name}` requires a value (`:set {name}=…`)"
        ));
    }
    let value = match action {
        Some(v) => v,
        // Toggle: read the live value through the same getter `?` uses.
        None => match (desc.get)(editor.settings()) {
            OptionValue::Bool(b) => !b,
            _ => return Err(format!("option `{name}` is not a boolean")),
        },
    };
    (desc.set)(editor.settings_mut(), OptionValue::Bool(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::make_editor;

    // ---- bare :set -----------------------------------------------------------

    #[test]
    fn set_bare_returns_info_with_shiftwidth() {
        let mut editor = make_editor();
        let result = apply_set(&mut editor, "");
        match result {
            ExEffect::Info(s) => assert!(
                s.contains("shiftwidth="),
                "bare :set info missing shiftwidth=, got: {s}"
            ),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    // ---- boolean options -----------------------------------------------------

    #[test]
    fn set_number_enables_number() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "number"), ExEffect::Ok);
        assert!(editor.settings().number);
    }

    #[test]
    fn set_nonumber_disables_number() {
        let mut editor = make_editor();
        editor.settings_mut().number = true;
        assert_eq!(apply_set(&mut editor, "nonumber"), ExEffect::Ok);
        assert!(!editor.settings().number);
    }

    #[test]
    fn set_nu_alias_enables_number() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "nu"), ExEffect::Ok);
        assert!(editor.settings().number);
    }

    #[test]
    fn set_nonu_alias_disables_number() {
        let mut editor = make_editor();
        editor.settings_mut().number = true;
        assert_eq!(apply_set(&mut editor, "nonu"), ExEffect::Ok);
        assert!(!editor.settings().number);
    }

    // ---- toggle (!) ---------------------------------------------------------

    #[test]
    fn set_number_bang_toggles_number_off() {
        let mut editor = make_editor();
        editor.settings_mut().number = true;
        assert_eq!(apply_set(&mut editor, "number!"), ExEffect::Ok);
        assert!(!editor.settings().number);
    }

    #[test]
    fn set_number_bang_toggles_number_on() {
        let mut editor = make_editor();
        editor.settings_mut().number = false;
        assert_eq!(apply_set(&mut editor, "number!"), ExEffect::Ok);
        assert!(editor.settings().number);
    }

    #[test]
    fn set_nu_bang_toggles_number() {
        let mut editor = make_editor();
        editor.settings_mut().number = true;
        assert_eq!(apply_set(&mut editor, "nu!"), ExEffect::Ok);
        assert!(!editor.settings().number);
    }

    // ---- numeric options ----------------------------------------------------

    #[test]
    fn set_tabstop_eq_5() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "tabstop=5"), ExEffect::Ok);
        assert_eq!(editor.settings().tabstop, 5);
    }

    #[test]
    fn set_scrolloff_eq_0() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "scrolloff=0"), ExEffect::Ok);
        assert_eq!(editor.settings().scrolloff, 0);
    }

    #[test]
    fn set_so_alias_sets_scrolloff() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "so=3"), ExEffect::Ok);
        assert_eq!(editor.settings().scrolloff, 3);
    }

    #[test]
    fn set_sidescrolloff_eq_5() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "sidescrolloff=5"), ExEffect::Ok);
        assert_eq!(editor.settings().sidescrolloff, 5);
    }

    #[test]
    fn set_siso_alias_sets_sidescrolloff() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "siso=2"), ExEffect::Ok);
        assert_eq!(editor.settings().sidescrolloff, 2);
    }

    #[test]
    fn set_scrolloff_query_returns_value() {
        let mut editor = make_editor();
        editor.settings_mut().scrolloff = 7;
        match apply_set(&mut editor, "so?") {
            ExEffect::Info(s) => assert_eq!(s, "so=7"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_sidescrolloff_query_returns_value() {
        let mut editor = make_editor();
        editor.settings_mut().sidescrolloff = 4;
        match apply_set(&mut editor, "siso?") {
            ExEffect::Info(s) => assert_eq!(s, "siso=4"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_scroll_duration_ms_roundtrip() {
        let mut editor = make_editor();
        assert_eq!(
            apply_set(&mut editor, "scroll_duration_ms=80"),
            ExEffect::Ok
        );
        assert_eq!(editor.settings().scroll_duration_ms, 80);
        match apply_set(&mut editor, "scroll_duration_ms?") {
            ExEffect::Info(s) => assert_eq!(s, "scroll_duration_ms=80"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_ts_alias_sets_tabstop() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "ts=4"), ExEffect::Ok);
        assert_eq!(editor.settings().tabstop, 4);
    }

    #[test]
    fn set_tabstop_eq_4() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "tabstop=4"), ExEffect::Ok);
        assert_eq!(editor.settings().tabstop, 4);
    }

    #[test]
    fn set_shiftwidth_eq_2() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "shiftwidth=2"), ExEffect::Ok);
        assert_eq!(editor.settings().shiftwidth, 2);
    }

    #[test]
    fn set_sw_alias_sets_shiftwidth() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "sw=8"), ExEffect::Ok);
        assert_eq!(editor.settings().shiftwidth, 8);
    }

    // ---- ignorecase / smartcase ---------------------------------------------

    #[test]
    fn set_ignorecase_enables() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "ignorecase"), ExEffect::Ok);
        assert!(editor.settings().ignore_case);
    }

    #[test]
    fn set_ic_alias_enables_ignorecase() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "ic"), ExEffect::Ok);
        assert!(editor.settings().ignore_case);
    }

    #[test]
    fn set_noic_disables_ignorecase() {
        let mut editor = make_editor();
        editor.settings_mut().ignore_case = true;
        assert_eq!(apply_set(&mut editor, "noic"), ExEffect::Ok);
        assert!(!editor.settings().ignore_case);
    }

    // ---- iskeyword ----------------------------------------------------------

    #[test]
    fn set_iskeyword_stored_verbatim() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "iskeyword=@,a-z"), ExEffect::Ok);
        assert_eq!(editor.settings().iskeyword, "@,a-z");
    }

    #[test]
    fn set_isk_alias_stores_iskeyword() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "isk=0-9"), ExEffect::Ok);
        assert_eq!(editor.settings().iskeyword, "0-9");
    }

    // ---- signcolumn ---------------------------------------------------------

    #[test]
    fn set_signcolumn_yes() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "signcolumn=yes"), ExEffect::Ok);
        assert_eq!(
            editor.settings().signcolumn,
            hjkl_engine::types::SignColumnMode::Yes
        );
    }

    #[test]
    fn set_scl_alias_auto() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "scl=auto"), ExEffect::Ok);
        assert_eq!(
            editor.settings().signcolumn,
            hjkl_engine::types::SignColumnMode::Auto
        );
    }

    #[test]
    fn set_scl_invalid_returns_error() {
        let mut editor = make_editor();
        let result = apply_set(&mut editor, "scl=invalid");
        assert!(
            matches!(result, ExEffect::Error(_)),
            "expected Error, got {result:?}"
        );
    }

    // ---- unknown option -----------------------------------------------------

    #[test]
    fn set_bad_name_returns_error_containing_name() {
        let mut editor = make_editor();
        let result = apply_set(&mut editor, "badname");
        match result {
            ExEffect::Error(s) => assert!(
                s.contains("badname"),
                "error should mention badname, got: {s}"
            ),
            other => panic!("expected Error(_), got {other:?}"),
        }
    }

    // ---- textwidth / tw alias ------------------------------------------------

    #[test]
    fn set_textwidth_and_tw_alias() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "textwidth=80"), ExEffect::Ok);
        assert_eq!(editor.settings().textwidth, 80);
        assert_eq!(apply_set(&mut editor, "tw=100"), ExEffect::Ok);
        assert_eq!(editor.settings().textwidth, 100);
    }

    // ---- numberwidth --------------------------------------------------------

    #[test]
    fn set_numberwidth_eq_6() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "numberwidth=6"), ExEffect::Ok);
        assert_eq!(editor.settings().numberwidth, 6);
    }

    #[test]
    fn set_nuw_alias_sets_numberwidth() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "nuw=3"), ExEffect::Ok);
        assert_eq!(editor.settings().numberwidth, 3);
    }

    // ---- relativenumber / rnu -----------------------------------------------

    #[test]
    fn set_relativenumber_enables() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "relativenumber"), ExEffect::Ok);
        assert!(editor.settings().relativenumber);
    }

    #[test]
    fn set_rnu_alias_enables_relativenumber() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "rnu"), ExEffect::Ok);
        assert!(editor.settings().relativenumber);
    }

    // ---- colorcolumn --------------------------------------------------------

    #[test]
    fn set_colorcolumn_stored() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "colorcolumn=80"), ExEffect::Ok);
        assert_eq!(editor.settings().colorcolumn, "80");
    }

    #[test]
    fn set_foldmarker_default_is_curly_triples() {
        let editor = make_editor();
        assert_eq!(editor.settings().foldmarker, "{{{,}}}");
    }

    #[test]
    fn set_foldmarker_stored() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "foldmarker=[[[,]]]"), ExEffect::Ok);
        assert_eq!(editor.settings().foldmarker, "[[[,]]]");
    }

    #[test]
    fn set_fmr_alias_stores_foldmarker() {
        let mut editor = make_editor();
        assert_eq!(
            apply_set(&mut editor, "fmr=#region,#endregion"),
            ExEffect::Ok
        );
        assert_eq!(editor.settings().foldmarker, "#region,#endregion");
    }

    #[test]
    fn set_cc_alias_stores_colorcolumn() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "cc=100"), ExEffect::Ok);
        assert_eq!(editor.settings().colorcolumn, "100");
    }

    // ---- background completion-only -----------------------------------------

    #[test]
    fn all_setting_names_contains_background() {
        let names = super::all_setting_names();
        assert!(
            names.iter().any(|n| n == "background"),
            "all_setting_names() must include \"background\" for :set Tab-completion"
        );
    }

    // ---- formatoptions -------------------------------------------------------

    #[test]
    fn set_formatoptions_equals() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "formatoptions=o"), ExEffect::Ok);
        assert_eq!(editor.settings().formatoptions, "o");
    }

    #[test]
    fn set_fo_alias() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "fo=r"), ExEffect::Ok);
        assert_eq!(editor.settings().formatoptions, "r");
    }

    #[test]
    fn set_fo_append_flag() {
        let mut editor = make_editor();
        editor.settings_mut().formatoptions = "r".to_string();
        assert_eq!(apply_set(&mut editor, "fo+=o"), ExEffect::Ok);
        assert!(editor.settings().formatoptions.contains('o'));
        assert!(editor.settings().formatoptions.contains('r'));
    }

    #[test]
    fn set_fo_remove_flag() {
        let mut editor = make_editor();
        editor.settings_mut().formatoptions = "ro".to_string();
        assert_eq!(apply_set(&mut editor, "fo-=r"), ExEffect::Ok);
        assert!(!editor.settings().formatoptions.contains('r'));
        assert!(editor.settings().formatoptions.contains('o'));
    }

    #[test]
    fn set_fo_append_no_duplicate() {
        let mut editor = make_editor();
        editor.settings_mut().formatoptions = "r".to_string();
        assert_eq!(apply_set(&mut editor, "fo+=r"), ExEffect::Ok);
        // Should not duplicate the `r` flag.
        assert_eq!(
            editor
                .settings()
                .formatoptions
                .chars()
                .filter(|&c| c == 'r')
                .count(),
            1
        );
    }

    // ---- filetype -----------------------------------------------------------

    #[test]
    fn set_filetype_stores_lang() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "filetype=rust"), ExEffect::Ok);
        assert_eq!(editor.settings().filetype, "rust");
    }

    #[test]
    fn set_ft_alias_stores_lang() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "ft=python"), ExEffect::Ok);
        assert_eq!(editor.settings().filetype, "python");
    }

    #[test]
    fn bare_set_shows_formatoptions_and_filetype() {
        let mut editor = make_editor();
        editor.settings_mut().filetype = "rust".to_string();
        let result = apply_set(&mut editor, "");
        match result {
            ExEffect::Info(s) => {
                assert!(
                    s.contains("formatoptions="),
                    "missing formatoptions in :set output"
                );
                assert!(s.contains("filetype="), "missing filetype in :set output");
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn all_setting_names_contains_formatoptions() {
        let names = super::all_setting_names();
        assert!(names.iter().any(|n| n == "formatoptions"));
        assert!(names.iter().any(|n| n == "fo"));
    }

    #[test]
    fn all_setting_names_contains_filetype() {
        let names = super::all_setting_names();
        assert!(names.iter().any(|n| n == "filetype"));
        assert!(names.iter().any(|n| n == "ft"));
    }

    // ── `:set <name>?` query form ────────────────────────────────────────

    #[test]
    fn set_filetype_query_returns_info() {
        let mut editor = make_editor();
        editor.settings_mut().filetype = "rust".to_string();
        match apply_set(&mut editor, "filetype?") {
            ExEffect::Info(s) => assert_eq!(s, "filetype=\"rust\""),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_ft_alias_query_works() {
        let mut editor = make_editor();
        editor.settings_mut().filetype = "html".to_string();
        match apply_set(&mut editor, "ft?") {
            ExEffect::Info(s) => assert_eq!(s, "ft=\"html\""),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_bool_query_reports_on_off() {
        let mut editor = make_editor();
        editor.settings_mut().autopair = true;
        match apply_set(&mut editor, "autopair?") {
            ExEffect::Info(s) => assert_eq!(s, "autopair=on"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    // ---- fixendofline / endofline -------------------------------------------

    /// `:set nofixeol` / `:set fixendofline` flip the engine setting, and
    /// `:set fixeol?` reports it — the vim default is ON.
    #[test]
    fn set_fixendofline_applies_and_queries_through_both_spellings() {
        let mut editor = make_editor();
        assert!(
            editor.settings().fixendofline,
            "vim's 'fixendofline' defaults on"
        );

        assert_eq!(apply_set(&mut editor, "nofixeol"), ExEffect::Ok);
        assert!(
            !editor.settings().fixendofline,
            "`nofixeol` alias must apply"
        );
        match apply_set(&mut editor, "fixendofline?") {
            ExEffect::Info(s) => assert_eq!(s, "fixendofline=off"),
            other => panic!("expected Info(_), got {other:?}"),
        }

        assert_eq!(apply_set(&mut editor, "fixendofline"), ExEffect::Ok);
        assert!(editor.settings().fixendofline);
        match apply_set(&mut editor, "fixeol?") {
            ExEffect::Info(s) => assert_eq!(s, "fixeol=on"),
            other => panic!("expected Info(_), got {other:?}"),
        }

        assert_eq!(apply_set(&mut editor, "nofixendofline"), ExEffect::Ok);
        assert!(!editor.settings().fixendofline);
    }

    /// `endofline` is host-owned buffer-local state (the TUI / headless /
    /// embed hosts intercept it before hjkl-ex). hjkl-ex must therefore accept
    /// the token silently rather than erroring, exactly like `background`.
    #[test]
    fn set_endofline_is_accepted_silently_for_the_host_to_own() {
        let mut editor = make_editor();
        for token in ["endofline", "eol", "noendofline", "noeol"] {
            assert_eq!(
                apply_set(&mut editor, token),
                ExEffect::Ok,
                "`:set {token}` must not error in hjkl-ex"
            );
        }
    }

    #[test]
    fn set_int_query_reports_number() {
        let mut editor = make_editor();
        editor.settings_mut().shiftwidth = 4;
        match apply_set(&mut editor, "sw?") {
            ExEffect::Info(s) => assert_eq!(s, "sw=4"),
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_unknown_query_errors() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "bogus?") {
            ExEffect::Error(s) => assert!(s.contains("bogus"), "got {s:?}"),
            other => panic!("expected Error(_), got {other:?}"),
        }
    }

    // ---- boolean options via `name=value` ------------------------------------

    #[test]
    fn set_bool_eq_off_disables() {
        let mut editor = make_editor();
        editor.settings_mut().format_on_save = true;
        assert_eq!(apply_set(&mut editor, "format_on_save=off"), ExEffect::Ok);
        assert!(
            !editor.settings().format_on_save,
            "`:set format_on_save=off` must disable it, not error"
        );
    }

    #[test]
    fn set_bool_eq_accepts_all_keywords() {
        for (val, exp) in [
            ("on", true),
            ("off", false),
            ("true", true),
            ("false", false),
            ("yes", true),
            ("no", false),
            ("1", true),
            ("0", false),
        ] {
            let mut editor = make_editor();
            assert_eq!(
                apply_set(&mut editor, &format!("format_on_save={val}")),
                ExEffect::Ok,
                "`=value` form must accept `{val}`"
            );
            assert_eq!(
                editor.settings().format_on_save,
                exp,
                "format_on_save={val} → {exp}"
            );
        }
    }

    #[test]
    fn set_bool_eq_bad_value_errors() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "format_on_save=maybe") {
            ExEffect::Error(s) => assert!(s.contains("bad value"), "got {s:?}"),
            other => panic!("expected Error for non-bool value, got {other:?}"),
        }
    }

    #[test]
    fn set_numeric_eq_still_works_alongside_bool_path() {
        // A numeric option with a `0`/`1` value must NOT be hijacked by the
        // boolean keyword path.
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "tabstop=1"), ExEffect::Ok);
        assert_eq!(editor.settings().tabstop, 1);
    }

    #[test]
    fn set_mixed_query_and_apply_returns_info() {
        let mut editor = make_editor();
        editor.settings_mut().filetype = "rust".to_string();
        // `:set number filetype?` — apply nu, then report ft.
        match apply_set(&mut editor, "number filetype?") {
            ExEffect::Info(s) => {
                assert_eq!(s, "filetype=\"rust\"");
                assert!(editor.settings().number, "number must be applied alongside");
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    // ---- tabline_icons (#200) -----------------------------------------------

    #[test]
    fn tabline_icons_default_on_and_toggles() {
        let mut editor = make_editor();
        assert!(editor.settings().tabline_icons, "tabline_icons defaults on");
        assert_eq!(apply_set(&mut editor, "notabline_icons"), ExEffect::Ok);
        assert!(!editor.settings().tabline_icons, ":set notabline_icons off");
        assert_eq!(apply_set(&mut editor, "tabline_icons"), ExEffect::Ok);
        assert!(editor.settings().tabline_icons, ":set tabline_icons on");
    }

    // ---- list / listchars ---------------------------------------------------

    #[test]
    fn set_list_enables() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "list"), ExEffect::Ok);
        assert!(editor.settings().list);
    }

    #[test]
    fn set_nolist_disables() {
        let mut editor = make_editor();
        editor.settings_mut().list = true;
        assert_eq!(apply_set(&mut editor, "nolist"), ExEffect::Ok);
        assert!(!editor.settings().list);
    }

    #[test]
    fn set_listchars_equals_stores_value() {
        let mut editor = make_editor();
        assert_eq!(
            apply_set(&mut editor, "listchars=tab:>-,eol:$"),
            ExEffect::Ok
        );
        let lc = &editor.settings().listchars;
        assert_eq!(lc.tab_lead, '>');
        assert_eq!(lc.tab_fill, Some('-'));
        assert_eq!(lc.eol, Some('$'));
    }

    #[test]
    fn set_lcs_alias_stores_listchars() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "lcs=tab:>-,trail:~"), ExEffect::Ok);
        assert_eq!(editor.settings().listchars.trail, Some('~'));
    }

    #[test]
    fn set_listchars_query_returns_value() {
        let mut editor = make_editor();
        assert_eq!(
            apply_set(&mut editor, "listchars=tab:>-,eol:$"),
            ExEffect::Ok
        );
        match apply_set(&mut editor, "listchars?") {
            ExEffect::Info(s) => {
                assert!(s.contains("tab:>-"), "query output must contain tab:>-");
                assert!(s.contains("eol:$"), "query output must contain eol:$");
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    #[test]
    fn set_listchars_invalid_value_returns_error() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "listchars=bogus:x") {
            ExEffect::Error(_) => {}
            other => panic!("expected Error(_), got {other:?}"),
        }
    }

    #[test]
    fn set_list_default_is_false() {
        let editor = make_editor();
        assert!(!editor.settings().list, "list default must be false");
    }

    // ---- blame_inline -------------------------------------------------------

    #[test]
    fn set_blame_inline_toggles() {
        let mut editor = make_editor();
        assert!(editor.settings().blame_inline, "default on");
        assert_eq!(apply_set(&mut editor, "noblame_inline"), ExEffect::Ok);
        assert!(!editor.settings().blame_inline, "no- turns off");
        assert_eq!(apply_set(&mut editor, "blame_inline"), ExEffect::Ok);
        assert!(editor.settings().blame_inline, "set on");
    }

    // ---- diagnostics_inline -------------------------------------------------

    #[test]
    fn set_diagnostics_inline_modes() {
        use hjkl_engine::types::DiagInlineMode;
        let mut editor = make_editor();
        assert_eq!(
            editor.settings().diagnostics_inline,
            DiagInlineMode::All,
            "default is all"
        );
        assert_eq!(
            apply_set(&mut editor, "diagnostics_inline=off"),
            ExEffect::Ok
        );
        assert_eq!(editor.settings().diagnostics_inline, DiagInlineMode::Off);
        assert_eq!(
            apply_set(&mut editor, "diaginline=current"),
            ExEffect::Ok,
            "alias + current mode"
        );
        assert_eq!(
            editor.settings().diagnostics_inline,
            DiagInlineMode::Current
        );
        assert_eq!(
            apply_set(&mut editor, "diagnostics_inline=all"),
            ExEffect::Ok
        );
        assert_eq!(editor.settings().diagnostics_inline, DiagInlineMode::All);
        // Invalid value is rejected.
        assert!(matches!(
            apply_set(&mut editor, "diagnostics_inline=bogus"),
            ExEffect::Error(_)
        ));
    }

    #[test]
    fn bare_set_output_contains_list() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "") {
            ExEffect::Info(s) => {
                assert!(
                    s.contains("list="),
                    "bare :set output must include list=, got: {s}"
                );
                assert!(
                    s.contains("listchars="),
                    "bare :set output must include listchars=, got: {s}"
                );
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }

    // ── foldmethod / foldenable / foldlevelstart ──────────────────────────────

    #[test]
    fn set_foldmethod_default_is_expr() {
        let editor = make_editor();
        assert_eq!(
            editor.settings().foldmethod,
            hjkl_engine::types::FoldMethod::Expr,
            "foldmethod default must be Expr"
        );
    }

    #[test]
    fn set_foldmethod_eq_manual() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "foldmethod=manual"), ExEffect::Ok);
        assert_eq!(
            editor.settings().foldmethod,
            hjkl_engine::types::FoldMethod::Manual
        );
    }

    #[test]
    fn set_fdm_alias_sets_foldmethod() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "fdm=marker"), ExEffect::Ok);
        assert_eq!(
            editor.settings().foldmethod,
            hjkl_engine::types::FoldMethod::Marker
        );
    }

    #[test]
    fn set_foldmethod_eq_expr() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "foldmethod=expr"), ExEffect::Ok);
        assert_eq!(
            editor.settings().foldmethod,
            hjkl_engine::types::FoldMethod::Expr
        );
    }

    #[test]
    fn set_foldmethod_syntax_is_alias_for_expr() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "foldmethod=syntax"), ExEffect::Ok);
        assert_eq!(
            editor.settings().foldmethod,
            hjkl_engine::types::FoldMethod::Expr,
            "foldmethod=syntax must map to Expr"
        );
    }

    #[test]
    fn set_foldmethod_invalid_returns_error() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "foldmethod=bogus") {
            ExEffect::Error(_) => {}
            other => panic!("expected Error(_), got {other:?}"),
        }
    }

    #[test]
    fn set_foldenable_default_is_true() {
        let editor = make_editor();
        assert!(
            editor.settings().foldenable,
            "foldenable default must be true"
        );
    }

    #[test]
    fn set_nofoldenable_disables_foldenable() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "nofoldenable"), ExEffect::Ok);
        assert!(!editor.settings().foldenable);
    }

    #[test]
    fn set_fen_alias_toggles_foldenable() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "nofen"), ExEffect::Ok);
        assert!(!editor.settings().foldenable);
        assert_eq!(apply_set(&mut editor, "fen"), ExEffect::Ok);
        assert!(editor.settings().foldenable);
    }

    #[test]
    fn set_foldlevelstart_default_is_99() {
        let editor = make_editor();
        assert_eq!(
            editor.settings().foldlevelstart,
            99,
            "foldlevelstart default must be 99"
        );
    }

    #[test]
    fn set_foldlevelstart_eq_0() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "foldlevelstart=0"), ExEffect::Ok);
        assert_eq!(editor.settings().foldlevelstart, 0);
    }

    #[test]
    fn set_fls_alias_sets_foldlevelstart() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "fls=5"), ExEffect::Ok);
        assert_eq!(editor.settings().foldlevelstart, 5);
    }

    // ── value + boolean completion metadata (issue #306) ─────────────────────

    #[test]
    fn setting_value_candidates_enum_options() {
        assert_eq!(
            setting_value_candidates("background"),
            vec!["dark", "light"]
        );
        assert_eq!(setting_value_candidates("bg"), vec!["dark", "light"]);
        assert_eq!(
            setting_value_candidates("signcolumn"),
            vec!["auto", "no", "yes"]
        );
        assert_eq!(
            setting_value_candidates("foldmethod"),
            vec!["expr", "manual", "marker", "syntax"]
        );
        assert_eq!(
            setting_value_candidates("diagnostics_inline"),
            vec!["all", "current", "off"]
        );
        // Aliases resolve to the same values.
        assert_eq!(
            setting_value_candidates("fdm"),
            setting_value_candidates("foldmethod")
        );
        assert_eq!(
            setting_value_candidates("scl"),
            setting_value_candidates("signcolumn")
        );
    }

    #[test]
    fn setting_value_candidates_non_enum_and_unknown_are_empty() {
        // Numeric, free-string, boolean, and unknown options have no enum values.
        assert!(setting_value_candidates("tabstop").is_empty());
        assert!(setting_value_candidates("filetype").is_empty());
        assert!(setting_value_candidates("number").is_empty());
        assert!(setting_value_candidates("bogus").is_empty());
    }

    #[test]
    fn boolean_setting_names_sanity() {
        let names = boolean_setting_names();
        assert!(names.iter().any(|n| n == "number"));
        assert!(names.iter().any(|n| n == "ignorecase"));
        assert!(names.iter().any(|n| n == "foldenable"));
        // Non-boolean options must NOT appear.
        assert!(!names.iter().any(|n| n == "tabstop"));
        assert!(!names.iter().any(|n| n == "foldmethod"));
        assert!(!names.iter().any(|n| n == "background"));
        // Canonical names only — no short aliases.
        assert!(!names.iter().any(|n| n == "nu"));
    }

    #[test]
    fn boolean_names_are_applyable() {
        // Guard against drift from the parser: every advertised boolean name
        // must apply both bare (`name`) and negated (`noname`) without error.
        for name in boolean_setting_names() {
            let mut editor = make_editor();
            assert_eq!(
                apply_set(&mut editor, &name),
                ExEffect::Ok,
                "`:set {name}` must succeed"
            );
            assert_eq!(
                apply_set(&mut editor, &format!("no{name}")),
                ExEffect::Ok,
                "`:set no{name}` must succeed"
            );
        }
    }

    #[test]
    fn bare_set_output_contains_foldmethod() {
        let mut editor = make_editor();
        match apply_set(&mut editor, "") {
            ExEffect::Info(s) => {
                assert!(
                    s.contains("foldmethod="),
                    "bare :set output must include foldmethod=, got: {s}"
                );
                assert!(
                    s.contains("foldenable="),
                    "bare :set output must include foldenable=, got: {s}"
                );
                assert!(
                    s.contains("foldlevelstart="),
                    "bare :set output must include foldlevelstart=, got: {s}"
                );
            }
            other => panic!("expected Info(_), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod registry_derived {
    use super::*;
    use hjkl_engine::options_registry::OptScope;

    /// Regression: `:set inv<name>` was offered by completion (there is a
    /// passing test for that in `complete.rs`) and rejected by the parser for
    /// EVERY boolean — the `inv` prefix had no arm at all. Driving all of them
    /// through the registry is what makes the two agree by construction.
    #[test]
    fn inv_prefix_toggles_every_boolean() {
        for d in OPTIONS.iter().filter(|d| d.kind == OptKind::Bool) {
            let mut ed = crate::test_util::make_editor();
            let OptionValue::Bool(before) = (d.get)(ed.settings()) else {
                unreachable!()
            };
            apply_set_token(&mut ed, &format!("inv{}", d.name))
                .unwrap_or_else(|e| panic!("`:set inv{}` failed: {e}", d.name));
            let OptionValue::Bool(after) = (d.get)(ed.settings()) else {
                unreachable!()
            };
            assert_eq!(after, !before, "`:set inv{}` did not toggle", d.name);
        }
    }

    /// Regression: `:set <name>!` was implemented for `number`,
    /// `relativenumber`, `cursorline` and `cursorcolumn`, and errored with
    /// "unknown :set option" for the other thirty booleans.
    #[test]
    fn bang_suffix_toggles_every_boolean() {
        for d in OPTIONS.iter().filter(|d| d.kind == OptKind::Bool) {
            let mut ed = crate::test_util::make_editor();
            let OptionValue::Bool(before) = (d.get)(ed.settings()) else {
                unreachable!()
            };
            apply_set_token(&mut ed, &format!("{}!", d.name))
                .unwrap_or_else(|e| panic!("`:set {}!` failed: {e}", d.name));
            let OptionValue::Bool(after) = (d.get)(ed.settings()) else {
                unreachable!()
            };
            assert_eq!(after, !before, "`:set {}!` did not toggle", d.name);
        }
    }

    /// Regression: `modifiable`, `linebreak` and `motion_sneak` had no `?`
    /// query arm, so `:set modifiable?` reported "unknown :set option".
    #[test]
    fn every_option_answers_a_query() {
        let ed = crate::test_util::make_editor();
        for d in OPTIONS {
            for name in d.all_names() {
                assert!(
                    query_option_value(&ed, name).is_some(),
                    "`:set {name}?` has no answer"
                );
            }
        }
    }

    /// `no` must not shadow an option whose own name starts with it —
    /// `number` is the trap, and the modeline parser fell into the same one.
    #[test]
    fn no_prefix_does_not_shadow_an_option_named_no_something() {
        let mut ed = crate::test_util::make_editor();
        ed.settings_mut().number = false;
        apply_set_token(&mut ed, "number").unwrap();
        assert!(ed.settings().number, "`:set number` must set, not clear");
        apply_set_token(&mut ed, "nonumber").unwrap();
        assert!(!ed.settings().number, "`:set nonumber` must clear");
    }

    /// A non-boolean given without a value gets a message naming the fix,
    /// rather than "unknown option" (which it is not).
    #[test]
    fn bare_non_boolean_reports_that_it_needs_a_value() {
        let mut ed = crate::test_util::make_editor();
        let err = apply_set_token(&mut ed, "scrolloff").unwrap_err();
        assert!(err.contains("requires a value"), "got: {err}");
    }

    /// Completion candidates for an enum come from the same slice the setter
    /// validates against, so nothing offered can be rejected.
    #[test]
    fn every_offered_enum_value_applies() {
        for d in OPTIONS {
            for name in d.all_names() {
                for v in setting_value_candidates(name) {
                    let mut ed = crate::test_util::make_editor();
                    apply_set_token(&mut ed, &format!("{name}={v}")).unwrap_or_else(|e| {
                        panic!("completion offers `:set {name}={v}` but it fails: {e}")
                    });
                }
            }
        }
    }

    /// The host-owned names must stay out of the registry — each is applied by
    /// the app before `:set` reaches the engine, and a `Settings` field for one
    /// would mean two owners for the same value.
    #[test]
    fn host_owned_names_are_not_in_the_registry() {
        for name in HOST_OWNED_NAMES {
            assert!(
                lookup(name).is_none(),
                "`{name}` is host-owned but also registered"
            );
        }
    }

    /// Buffer-local options are still settable — they just never persist.
    #[test]
    fn buffer_local_options_remain_settable() {
        let mut ed = crate::test_util::make_editor();
        apply_set_token(&mut ed, "filetype=rust").unwrap();
        assert_eq!(ed.settings().filetype, "rust");
        assert!(
            OPTIONS
                .iter()
                .any(|d| d.name == "filetype" && d.scope == OptScope::BufferLocal)
        );
    }
}
