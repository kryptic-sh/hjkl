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
        "foldlevelstart".into(),
        "fls".into(),
        "scrolloff".into(),
        "so".into(),
        "sidescrolloff".into(),
        "siso".into(),
        "scroll_duration_ms".into(),
        // string (fold-related)
        "foldmethod".into(),
        "fdm".into(),
        "foldmarker".into(),
        "fmr".into(),
        // string
        "listchars".into(),
        "lcs".into(),
        "iskeyword".into(),
        "isk".into(),
        "signcolumn".into(),
        "scl".into(),
        "colorcolumn".into(),
        "cc".into(),
        "formatoptions".into(),
        "fo".into(),
        "filetype".into(),
        "ft".into(),
        "commentstring".into(),
        "cms".into(),
        // completion-only (handled by host in ex_dispatch.rs)
        "background".into(),
        "bg".into(),
        "list".into(),
        "blame_inline".into(),
        "diagnostics_inline".into(),
        "diaginline".into(),
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
        "autopair".into(),
        "ap".into(),
        "autoclose-tag".into(),
        "act".into(),
        "autoreload".into(),
        "ar".into(),
        "indent_guides".into(),
        "ig".into(),
        "indent_guide_char".into(),
        "igc".into(),
        "format_on_save".into(),
        "fos".into(),
        "trim_trailing_whitespace".into(),
        "tts".into(),
        "rainbow_brackets".into(),
        "rb".into(),
        "matchparen".into(),
        "mps".into(),
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
        let fdm = match s.foldmethod {
            hjkl_engine::types::FoldMethod::Manual => "manual",
            hjkl_engine::types::FoldMethod::Expr => "expr",
            hjkl_engine::types::FoldMethod::Marker => "marker",
        };
        return ExEffect::Info(format!(
            "shiftwidth={}  tabstop={}  softtabstop={}  textwidth={}  undolevels={}  timeoutlen={}  iskeyword=\"{}\"  expandtab={}  ignorecase={}  smartcase={}  wrapscan={}  autoindent={}  smartindent={}  undobreak={}  readonly={}  wrap={}  number={}  relativenumber={}  numberwidth={}  cursorline={}  cursorcolumn={}  signcolumn={}  foldcolumn={}  foldmethod={}  foldenable={}  foldlevelstart={}  colorcolumn=\"{}\"  formatoptions=\"{}\"  filetype=\"{}\"  commentstring=\"{}\"  autopair={}  autoclose-tag={}  scrolloff={}  sidescrolloff={}  list={}  listchars=\"{}\"  indent_guides={}  indent_guide_char={}  format_on_save={}  trim_trailing_whitespace={}  rainbow_brackets={}  matchparen={}  autoreload={}  scroll_duration_ms={}",
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
/// form. Names accepted match those in [`apply_set_token`] (both the
/// long and short aliases).
fn query_option_value<H: Host>(
    editor: &hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    name: &str,
) -> Option<String> {
    let s = editor.settings();
    let on_off = |b: bool| if b { "on" } else { "off" }.to_string();
    Some(match name {
        "shiftwidth" | "sw" => s.shiftwidth.to_string(),
        "tabstop" | "ts" => s.tabstop.to_string(),
        "softtabstop" | "sts" => s.softtabstop.to_string(),
        "textwidth" | "tw" => s.textwidth.to_string(),
        "undolevels" | "ul" => s.undo_levels.to_string(),
        "timeoutlen" | "tm" => s.timeout_len.as_millis().to_string(),
        "numberwidth" | "nuw" => s.numberwidth.to_string(),
        "foldcolumn" | "fdc" => s.foldcolumn.to_string(),
        "foldmethod" | "fdm" => match s.foldmethod {
            hjkl_engine::types::FoldMethod::Manual => "manual".to_string(),
            hjkl_engine::types::FoldMethod::Expr => "expr".to_string(),
            hjkl_engine::types::FoldMethod::Marker => "marker".to_string(),
        },
        "foldenable" | "fen" => on_off(s.foldenable),
        "foldmarker" | "fmr" => format!("\"{}\"", s.foldmarker),
        "foldlevelstart" | "fls" => s.foldlevelstart.to_string(),
        "scrolloff" | "so" => s.scrolloff.to_string(),
        "sidescrolloff" | "siso" => s.sidescrolloff.to_string(),
        "scroll_duration_ms" => s.scroll_duration_ms.to_string(),
        "iskeyword" | "isk" => format!("\"{}\"", s.iskeyword),
        "colorcolumn" | "cc" => format!("\"{}\"", s.colorcolumn),
        "formatoptions" | "fo" => format!("\"{}\"", s.formatoptions),
        "filetype" | "ft" => format!("\"{}\"", s.filetype),
        "commentstring" | "cms" => format!("\"{}\"", s.commentstring),
        "signcolumn" | "scl" => match s.signcolumn {
            hjkl_engine::types::SignColumnMode::Yes => "yes".into(),
            hjkl_engine::types::SignColumnMode::No => "no".into(),
            hjkl_engine::types::SignColumnMode::Auto => "auto".into(),
        },
        "wrap" => match s.wrap {
            hjkl_buffer::Wrap::None => "off".into(),
            hjkl_buffer::Wrap::Char => "char".into(),
            hjkl_buffer::Wrap::Word => "word".into(),
        },
        "expandtab" | "et" => on_off(s.expandtab),
        "ignorecase" | "ic" => on_off(s.ignore_case),
        "smartcase" | "scs" => on_off(s.smartcase),
        "wrapscan" | "ws" => on_off(s.wrapscan),
        "autoindent" | "ai" => on_off(s.autoindent),
        "autoreload" | "ar" => on_off(s.autoreload),
        "smartindent" | "si" => on_off(s.smartindent),
        "undobreak" => on_off(s.undo_break_on_motion),
        "readonly" | "ro" => on_off(s.readonly),
        "number" | "nu" => on_off(s.number),
        "relativenumber" | "rnu" => on_off(s.relativenumber),
        "cursorline" | "cul" => on_off(s.cursorline),
        "cursorcolumn" | "cuc" => on_off(s.cursorcolumn),
        "autopair" | "ap" => on_off(s.autopair),
        "autoclose-tag" | "act" => on_off(s.autoclose_tag),
        "list" => on_off(s.list),
        "blame_inline" => on_off(s.blame_inline),
        "diagnostics_inline" | "diaginline" => match s.diagnostics_inline {
            hjkl_engine::types::DiagInlineMode::Off => "off".into(),
            hjkl_engine::types::DiagInlineMode::Current => "current".into(),
            hjkl_engine::types::DiagInlineMode::All => "all".into(),
        },
        "listchars" | "lcs" => format!("\"{}\"", s.listchars.to_canonical_string()),
        "indent_guides" | "ig" => on_off(s.indent_guides),
        "indent_guide_char" | "igc" => s.indent_guide_char.to_string(),
        "format_on_save" | "fos" => on_off(s.format_on_save),
        "trim_trailing_whitespace" | "tts" => on_off(s.trim_trailing_whitespace),
        "rainbow_brackets" | "rb" => on_off(s.rainbow_brackets),
        "matchparen" | "mps" => on_off(s.matchparen),
        _ => return None,
    })
}

/// Apply a single `:set` token. Supports `name=value`, `name+=flags`,
/// `name-=flags`, bare `name` (turns booleans on), and `noname`
/// (turns booleans off).
fn apply_set_token<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    token: &str,
) -> Result<(), String> {
    // `formatoptions+=flags` — append flags.
    if let Some(rest) = token
        .strip_prefix("formatoptions+=")
        .or_else(|| token.strip_prefix("fo+="))
    {
        for ch in rest.chars() {
            if !editor.settings().formatoptions.contains(ch) {
                editor.settings_mut().formatoptions.push(ch);
            }
        }
        return Ok(());
    }
    // `formatoptions-=flags` — remove flags.
    if let Some(rest) = token
        .strip_prefix("formatoptions-=")
        .or_else(|| token.strip_prefix("fo-="))
    {
        for ch in rest.chars() {
            let fo = editor.settings().formatoptions.clone();
            editor.settings_mut().formatoptions = fo.chars().filter(|&c| c != ch).collect();
        }
        return Ok(());
    }

    if let Some((name, value)) = token.split_once('=') {
        // String-valued options short-circuit the numeric parse.
        if matches!(name, "iskeyword" | "isk") {
            editor.set_iskeyword(value);
            return Ok(());
        }
        if matches!(name, "listchars" | "lcs") {
            let lc = hjkl_buffer::ListChars::parse(value)?;
            editor.settings_mut().listchars = lc;
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
        if matches!(name, "diagnostics_inline" | "diaginline") {
            editor.settings_mut().diagnostics_inline = match value {
                "off" | "no" | "disable" | "disabled" => hjkl_engine::types::DiagInlineMode::Off,
                "current" | "cursor" | "line" => hjkl_engine::types::DiagInlineMode::Current,
                "all" | "on" | "enable" | "enabled" => hjkl_engine::types::DiagInlineMode::All,
                other => {
                    return Err(format!(
                        "diagnostics_inline must be `off`, `current`, or `all`, got `{other}`"
                    ));
                }
            };
            return Ok(());
        }
        if matches!(name, "foldmethod" | "fdm") {
            editor.settings_mut().foldmethod = match value {
                "manual" => hjkl_engine::types::FoldMethod::Manual,
                "expr" | "syntax" => hjkl_engine::types::FoldMethod::Expr,
                "marker" => hjkl_engine::types::FoldMethod::Marker,
                other => {
                    return Err(format!(
                        "foldmethod must be `manual`, `expr`, `syntax`, or `marker`, got `{other}`"
                    ));
                }
            };
            return Ok(());
        }
        if matches!(name, "colorcolumn" | "cc") {
            editor.settings_mut().colorcolumn = value.to_string();
            return Ok(());
        }
        if matches!(name, "foldmarker" | "fmr") {
            // Stored verbatim as `open,close`; validated (comma-separated,
            // both sides non-empty) at fold-extraction time, falling back to
            // the vim default `{{{,}}}` when malformed.
            editor.settings_mut().foldmarker = value.to_string();
            return Ok(());
        }
        if matches!(name, "formatoptions" | "fo") {
            editor.settings_mut().formatoptions = value.to_string();
            return Ok(());
        }
        if matches!(name, "filetype" | "ft") {
            editor.settings_mut().filetype = value.to_string();
            return Ok(());
        }
        if matches!(name, "commentstring" | "cms") {
            editor.settings_mut().commentstring = value.to_string();
            return Ok(());
        }
        if matches!(name, "indent_guide_char" | "igc") {
            let mut chars = value.chars();
            let ch = match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => {
                    return Err(format!(
                        "indent_guide_char expects exactly one character, got {value:?}"
                    ));
                }
            };
            editor.settings_mut().indent_guide_char = ch;
            return Ok(());
        }
        // Boolean options accept `name=on|off|true|false|yes|no|1|0`. Try this
        // before the numeric parse so `:set format_on_save=off` works (vim sets
        // booleans via `:set name`/`:set noname`, but `=value` is a common
        // expectation). A non-boolean name falls through to the numeric parse,
        // and `0`/`1` on a numeric option (e.g. `tabstop=1`) isn't a bool option
        // so it falls through too.
        if let Some(b) = parse_bool_word(value)
            && apply_bool_option(editor, name, b)
        {
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
            "foldlevelstart" | "fls" => {
                editor.settings_mut().foldlevelstart = parsed.min(u32::MAX as usize) as u32;
            }
            "scrolloff" | "so" => {
                editor.settings_mut().scrolloff = parsed;
            }
            "sidescrolloff" | "siso" => {
                editor.settings_mut().sidescrolloff = parsed;
            }
            "scroll_duration_ms" => {
                editor.settings_mut().scroll_duration_ms = parsed.min(u16::MAX as usize) as u16;
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
    if apply_bool_option(editor, name, value) {
        Ok(())
    } else {
        Err(format!("unknown :set option `{name}`"))
    }
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

/// Set boolean option `name` to `value`. Returns `true` when `name` is a known
/// boolean option (and was applied), `false` otherwise. Shared by the bare
/// `name`/`noname` path and the `name=on|off|true|false|yes|no|1|0` path.
fn apply_bool_option<H: Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    name: &str,
    value: bool,
) -> bool {
    match name {
        "ignorecase" | "ic" => editor.settings_mut().ignore_case = value,
        "smartcase" | "scs" => editor.settings_mut().smartcase = value,
        "wrapscan" | "ws" => editor.settings_mut().wrapscan = value,
        "expandtab" | "et" => editor.settings_mut().expandtab = value,
        "autoindent" | "ai" => editor.settings_mut().autoindent = value,
        "autoreload" | "ar" => editor.settings_mut().autoreload = value,
        "smartindent" | "si" => editor.settings_mut().smartindent = value,
        "undobreak" => editor.settings_mut().undo_break_on_motion = value,
        "readonly" | "ro" => editor.settings_mut().readonly = value,
        "modifiable" | "ma" => editor.settings_mut().modifiable = value,
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
        // NOTE: `background` is completion-only — the host intercepts it in
        // ex_dispatch.rs before hjkl-ex is consulted. Accept silently here so
        // hjkl-ex never emits an "unknown option" error if the token somehow
        // reaches this path.
        "autopair" | "ap" => editor.settings_mut().autopair = value,
        "autoclose-tag" | "act" => editor.settings_mut().autoclose_tag = value,
        "motion_sneak" | "snk" => editor.settings_mut().motion_sneak = value,
        "list" => editor.settings_mut().list = value,
        "blame_inline" => editor.settings_mut().blame_inline = value,
        "indent_guides" | "ig" => editor.settings_mut().indent_guides = value,
        "format_on_save" | "fos" => editor.settings_mut().format_on_save = value,
        "trim_trailing_whitespace" | "tts" => {
            editor.settings_mut().trim_trailing_whitespace = value
        }
        "rainbow_brackets" | "rb" => editor.settings_mut().rainbow_brackets = value,
        "matchparen" | "mps" => editor.settings_mut().matchparen = value,
        "foldenable" | "fen" => editor.settings_mut().foldenable = value,
        "background" | "bg" => {}
        _ => return false,
    }
    true
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
