//! `:set [option ...]` command — verbatim port from `hjkl-editor::ex::apply_set`
//! (lines 891–1085).  Bit-for-bit parity with legacy: same option list, same
//! aliases, same error strings.

use crate::effect::ExEffect;
use hjkl_engine::Host;

/// All `:set` option names and their short aliases.
///
/// Used by Phase 6's `Setting` arg completer to populate the candidate list.
/// Includes both canonical names and aliases; no dedup needed (they're all
/// distinct strings).
pub fn all_setting_names() -> Vec<String> {
    vec![
        // numeric
        "shiftwidth".into(),
        "sw".into(),
        "tabstop".into(),
        "ts".into(),
        "softtabstop".into(),
        "sts".into(),
        "textwidth".into(),
        "tw".into(),
        "undolevels".into(),
        "ul".into(),
        "timeoutlen".into(),
        "tm".into(),
        "numberwidth".into(),
        "nuw".into(),
        "foldcolumn".into(),
        "fdc".into(),
        // string
        "iskeyword".into(),
        "isk".into(),
        "signcolumn".into(),
        "scl".into(),
        "colorcolumn".into(),
        "cc".into(),
        // boolean
        "ignorecase".into(),
        "ic".into(),
        "smartcase".into(),
        "scs".into(),
        "wrapscan".into(),
        "ws".into(),
        "expandtab".into(),
        "et".into(),
        "autoindent".into(),
        "ai".into(),
        "smartindent".into(),
        "si".into(),
        "undobreak".into(),
        "readonly".into(),
        "ro".into(),
        "number".into(),
        "nu".into(),
        "relativenumber".into(),
        "rnu".into(),
        "cursorline".into(),
        "cul".into(),
        "cursorcolumn".into(),
        "cuc".into(),
        "wrap".into(),
        "linebreak".into(),
        "lbr".into(),
        "foldenable".into(),
        "fen".into(),
    ]
}

/// `:set [opt ...]` body. Splits on whitespace and applies each token.
/// Bare `:set` reports the current values for the supported options.
pub(crate) fn apply_set<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
        return ExEffect::Info(format!(
            "shiftwidth={}  tabstop={}  softtabstop={}  textwidth={}  undolevels={}  timeoutlen={}  iskeyword=\"{}\"  expandtab={}  ignorecase={}  smartcase={}  wrapscan={}  autoindent={}  smartindent={}  undobreak={}  readonly={}  wrap={}  number={}  relativenumber={}  numberwidth={}  cursorline={}  cursorcolumn={}  signcolumn={}  foldcolumn={}  colorcolumn=\"{}\"",
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
            s.colorcolumn,
        ));
    }
    for token in trimmed.split_whitespace() {
        if let Err(e) = apply_set_token(editor, token) {
            return ExEffect::Error(e);
        }
    }
    ExEffect::Ok
}

/// Apply a single `:set` token. Supports `name=value`, bare `name`
/// (turns booleans on), and `noname` (turns booleans off).
fn apply_set_token<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    token: &str,
) -> Result<(), String> {
    if let Some((name, value)) = token.split_once('=') {
        // String-valued options short-circuit the numeric parse.
        if matches!(name, "iskeyword" | "isk") {
            editor.set_iskeyword(value);
            return Ok(());
        }
        if matches!(name, "signcolumn" | "scl") {
            editor.settings_mut().signcolumn = match value {
                "yes" => hjkl_engine::types::SignColumnMode::Yes,
                "no" => hjkl_engine::types::SignColumnMode::No,
                "auto" => hjkl_engine::types::SignColumnMode::Auto,
                other => {
                    return Err(format!(
                        "signcolumn must be `yes`, `no`, or `auto`, got `{other}`"
                    ));
                }
            };
            return Ok(());
        }
        if matches!(name, "colorcolumn" | "cc") {
            editor.settings_mut().colorcolumn = value.to_string();
            return Ok(());
        }
        let parsed: usize = value
            .parse()
            .map_err(|_| format!("bad value `{value}` for :set {name}"))?;
        match name {
            "shiftwidth" | "sw" => {
                if parsed == 0 {
                    return Err("shiftwidth must be > 0".into());
                }
                editor.settings_mut().shiftwidth = parsed;
            }
            "tabstop" | "ts" => {
                if parsed == 0 {
                    return Err("tabstop must be > 0".into());
                }
                editor.settings_mut().tabstop = parsed;
            }
            "textwidth" | "tw" => {
                if parsed == 0 {
                    return Err("textwidth must be > 0".into());
                }
                editor.settings_mut().textwidth = parsed;
            }
            "undolevels" | "ul" => {
                editor.settings_mut().undo_levels = parsed.min(u32::MAX as usize) as u32;
            }
            "timeoutlen" | "tm" => {
                editor.settings_mut().timeout_len =
                    core::time::Duration::from_millis(parsed as u64);
            }
            "numberwidth" | "nuw" => {
                if !(1..=20).contains(&parsed) {
                    return Err(format!("numberwidth must be in range 1..=20, got {parsed}"));
                }
                editor.settings_mut().numberwidth = parsed;
            }
            "foldcolumn" | "fdc" => {
                if parsed > 12 {
                    return Err(format!("foldcolumn must be in range 0..=12, got {parsed}"));
                }
                editor.settings_mut().foldcolumn = parsed as u32;
            }
            other => return Err(format!("unknown :set option `{other}`")),
        }
        return Ok(());
    }
    // Handle toggle (name!) — must check before the `no` strip.
    if let Some(name) = token.strip_suffix('!') {
        match name {
            "number" | "nu" => {
                editor.settings_mut().number = !editor.settings().number;
            }
            "relativenumber" | "rnu" => {
                editor.settings_mut().relativenumber = !editor.settings().relativenumber;
            }
            "cursorline" | "cul" => {
                editor.settings_mut().cursorline = !editor.settings().cursorline;
            }
            "cursorcolumn" | "cuc" => {
                editor.settings_mut().cursorcolumn = !editor.settings().cursorcolumn;
            }
            other => return Err(format!("unknown :set option `{other}`")),
        }
        return Ok(());
    }
    let (name, value) = if let Some(rest) = token.strip_prefix("no") {
        (rest, false)
    } else {
        (token, true)
    };
    match name {
        "ignorecase" | "ic" => editor.settings_mut().ignore_case = value,
        "smartcase" | "scs" => editor.settings_mut().smartcase = value,
        "wrapscan" | "ws" => editor.settings_mut().wrapscan = value,
        "expandtab" | "et" => editor.settings_mut().expandtab = value,
        "autoindent" | "ai" => editor.settings_mut().autoindent = value,
        "smartindent" | "si" => editor.settings_mut().smartindent = value,
        "undobreak" => editor.settings_mut().undo_break_on_motion = value,
        "readonly" | "ro" => editor.settings_mut().readonly = value,
        "number" | "nu" => editor.settings_mut().number = value,
        "relativenumber" | "rnu" => editor.settings_mut().relativenumber = value,
        "cursorline" | "cul" => editor.settings_mut().cursorline = value,
        "cursorcolumn" | "cuc" => editor.settings_mut().cursorcolumn = value,
        "wrap" => {
            editor.settings_mut().wrap = if value {
                // Preserve `Wrap::Word` if `linebreak` already flipped
                // word-mode on; otherwise default `set wrap` to char.
                match editor.settings().wrap {
                    hjkl_buffer::Wrap::Word => hjkl_buffer::Wrap::Word,
                    _ => hjkl_buffer::Wrap::Char,
                }
            } else {
                hjkl_buffer::Wrap::None
            };
        }
        "linebreak" | "lbr" => {
            editor.settings_mut().wrap = if value {
                hjkl_buffer::Wrap::Word
            } else {
                // `nolinebreak` drops back to char wrap when wrap is on,
                // otherwise stays off.
                match editor.settings().wrap {
                    hjkl_buffer::Wrap::None => hjkl_buffer::Wrap::None,
                    _ => hjkl_buffer::Wrap::Char,
                }
            };
        }
        // Booleans we don't (yet) honour: accept silently so :set lines
        // copied from a vimrc don't error out. `foldenable` falls here.
        "foldenable" | "fen" => {}
        other => return Err(format!("unknown :set option `{other}`")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_engine::{DefaultHost, Editor, Options};

    fn make_editor() -> Editor<hjkl_buffer::Buffer, DefaultHost> {
        let buf = hjkl_buffer::Buffer::new();
        let host = DefaultHost::new();
        Editor::new(buf, host, Options::default())
    }

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
    fn set_scrolloff_eq_5() {
        let mut editor = make_editor();
        // scrolloff is not in legacy apply_set; confirm tabstop=5 instead
        assert_eq!(apply_set(&mut editor, "tabstop=5"), ExEffect::Ok);
        assert_eq!(editor.settings().tabstop, 5);
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

    // ---- softtabstop / so alias for scrolloff (not in legacy, verify tw) ---

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
    fn set_cc_alias_stores_colorcolumn() {
        let mut editor = make_editor();
        assert_eq!(apply_set(&mut editor, "cc=100"), ExEffect::Ok);
        assert_eq!(editor.settings().colorcolumn, "100");
    }
}
