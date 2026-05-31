//! Phase 4a–4d2 host-registry commands: app-level ex commands that need `&mut App`.
//!
//! Phase 4e will add more commands here.

use std::sync::LazyLock;

use hjkl_ex::{ArgKind, ExEffect, HostCmd};

use super::App;

/// `:tabnext` / `:tabn` — cycle to the next tab, wrapping around.
pub(crate) struct TabNextCmd;

impl HostCmd<App> for TabNextCmd {
    fn name(&self) -> &'static str {
        "tabnext"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabn"]
    }

    /// Vim accepts `:tabn` (4 chars); match that minimum.
    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabnext();
        Some(ExEffect::Ok)
    }
}

// ── Phase 4b commands ────────────────────────────────────────────────────────

/// `:split` / `:sp` — open a horizontal split (optional file arg).
pub(crate) struct SplitCmd;

impl HostCmd<App> for SplitCmd {
    fn name(&self) -> &'static str {
        "split"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["sp"]
    }

    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Path
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        app.do_split(args.trim());
        Some(ExEffect::Ok)
    }
}

/// `:vsplit` / `:vsp` — open a vertical split (optional file arg).
pub(crate) struct VsplitCmd;

impl HostCmd<App> for VsplitCmd {
    fn name(&self) -> &'static str {
        "vsplit"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["vsp"]
    }

    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Path
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        app.do_vsplit(args.trim());
        Some(ExEffect::Ok)
    }
}

/// `:close` / `:clo` — close the focused window.
pub(crate) struct CloseCmd;

impl HostCmd<App> for CloseCmd {
    fn name(&self) -> &'static str {
        "close"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["clo"]
    }

    fn min_prefix(&self) -> usize {
        3
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.close_focused_window();
        Some(ExEffect::Ok)
    }
}

/// `:tabnew` — open a new tab (optional file arg).
pub(crate) struct TabnewCmd;

impl HostCmd<App> for TabnewCmd {
    fn name(&self) -> &'static str {
        "tabnew"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabedit", "tabe"]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Path
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        app.do_tabnew(args.trim());
        Some(ExEffect::Ok)
    }
}

/// `:tabprev` / `:tabp` / `:tabN` — cycle to the previous tab, wrapping.
pub(crate) struct TabprevCmd;

impl HostCmd<App> for TabprevCmd {
    fn name(&self) -> &'static str {
        "tabprev"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabp", "tabN"]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabprev();
        Some(ExEffect::Ok)
    }
}

/// `:tabclose` / `:tabc` — close the current tab.
pub(crate) struct TabcloseCmd;

impl HostCmd<App> for TabcloseCmd {
    fn name(&self) -> &'static str {
        "tabclose"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabc"]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabclose();
        Some(ExEffect::Ok)
    }
}

/// `:tabmove` — reorder the current tab (optional N/+N/-N arg).
pub(crate) struct TabmoveCmd;

impl HostCmd<App> for TabmoveCmd {
    fn name(&self) -> &'static str {
        "tabmove"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        app.do_tabmove(args.trim());
        Some(ExEffect::Ok)
    }
}

/// `:only` / `:on` — close all windows except the focused one.
pub(crate) struct OnlyCmd;

impl HostCmd<App> for OnlyCmd {
    fn name(&self) -> &'static str {
        "only"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["on"]
    }

    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.only_focused_window();
        Some(ExEffect::Ok)
    }
}

// ── Phase 4c commands ────────────────────────────────────────────────────────

/// `:bnext` / `:bn` — advance to the next buffer slot, wrapping.
pub(crate) struct BnextCmd;

impl HostCmd<App> for BnextCmd {
    fn name(&self) -> &'static str {
        "bnext"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["bn"]
    }

    /// Vim accepts `:bn` (2 chars); COMMAND_NAMES confirms min_prefix=2.
    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.buffer_next();
        Some(ExEffect::Ok)
    }
}

/// `:bprevious` / `:bp` / `:bNext` — retreat to the previous buffer slot, wrapping.
pub(crate) struct BprevCmd;

impl HostCmd<App> for BprevCmd {
    fn name(&self) -> &'static str {
        "bprevious"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["bp", "bNext"]
    }

    /// COMMAND_NAMES: bprevious min_prefix=2; bNext min_prefix=2.
    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.buffer_prev();
        Some(ExEffect::Ok)
    }
}

/// `:bfirst` / `:bf` — jump to the first buffer slot (index 0).
pub(crate) struct BfirstCmd;

impl HostCmd<App> for BfirstCmd {
    fn name(&self) -> &'static str {
        "bfirst"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["bf"]
    }

    /// COMMAND_NAMES: bfirst min_prefix=2.
    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.switch_to(0);
        Some(ExEffect::Ok)
    }
}

/// `:blast` / `:bl` — jump to the last buffer slot.
pub(crate) struct BlastCmd;

impl HostCmd<App> for BlastCmd {
    fn name(&self) -> &'static str {
        "blast"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["bl"]
    }

    /// COMMAND_NAMES: blast min_prefix=2.
    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        let last = app.slots.len().saturating_sub(1);
        app.switch_to(last);
        Some(ExEffect::Ok)
    }
}

/// `:buffers` / `:ls` / `:files` — display the buffer list in the status area.
pub(crate) struct BuffersCmd;

impl HostCmd<App> for BuffersCmd {
    fn name(&self) -> &'static str {
        "buffers"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["ls", "files"]
    }

    /// COMMAND_NAMES: buffers/ls/files all min_prefix=2.
    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        let info = app.list_buffers();
        Some(ExEffect::Info(info))
    }
}

/// `:clipboard` — display the clipboard backend capabilities.
/// Not in COMMAND_NAMES; matched by exact name only.
pub(crate) struct ClipboardCmd;

impl HostCmd<App> for ClipboardCmd {
    fn name(&self) -> &'static str {
        "clipboard"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Not in COMMAND_NAMES; use full-name length as min_prefix (no abbreviation).
    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        let info = app.clipboard_status();
        Some(ExEffect::Info(info))
    }
}

// ── Phase 4d2 commands ───────────────────────────────────────────────────────

/// `:picker` — open the fuzzy file picker.
pub(crate) struct PickerCmd;

impl HostCmd<App> for PickerCmd {
    fn name(&self) -> &'static str {
        "picker"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        6
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.open_picker();
        Some(ExEffect::Ok)
    }
}

/// `:rg [pattern]` — open the ripgrep content-search picker.
pub(crate) struct RgCmd;

impl HostCmd<App> for RgCmd {
    fn name(&self) -> &'static str {
        "rg"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        2
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Path
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        let pattern = if args.is_empty() { None } else { Some(args) };
        app.open_grep_picker(pattern);
        Some(ExEffect::Ok)
    }
}

/// `:b [num|name]` — switch to a buffer by 1-based index or filename fragment.
pub(crate) struct BCmd;

impl HostCmd<App> for BCmd {
    fn name(&self) -> &'static str {
        "b"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// min_prefix=1 so `:b <arg>` resolves; `:b#` is excluded by exact match
    /// (the legacy arm keeps that).
    fn min_prefix(&self) -> usize {
        1
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Buffer
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        let arg = args.trim();
        if arg.is_empty() {
            return Some(ExEffect::Error("E94: No matching buffer".into()));
        }
        if arg.chars().all(|c| c.is_ascii_digit()) {
            let n: usize = arg.parse().unwrap_or(0);
            if n == 0 || n > app.slots.len() {
                return Some(ExEffect::Error(format!("E86: Buffer {n} does not exist")));
            }
            app.switch_to(n - 1);
        } else {
            let arg_lower = arg.to_lowercase();
            let matches: Vec<usize> = app
                .slots
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.filename
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .map(|n| n.to_lowercase().contains(&arg_lower))
                        .unwrap_or(false)
                })
                .map(|(i, _)| i)
                .collect();
            match matches.len() {
                0 => {
                    return Some(ExEffect::Error(format!(
                        "E94: No matching buffer for {arg}"
                    )));
                }
                1 => {
                    app.switch_to(matches[0]);
                }
                _ => {
                    return Some(ExEffect::Error(format!(
                        "E93: More than one match for {arg}"
                    )));
                }
            }
        }
        Some(ExEffect::Ok)
    }
}

/// `:bpicker` — open the buffer picker.
pub(crate) struct BpickerCmd;

impl HostCmd<App> for BpickerCmd {
    fn name(&self) -> &'static str {
        "bpicker"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        6
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.open_buffer_picker();
        Some(ExEffect::Ok)
    }
}

/// `:checktime` — reload buffers that changed on disk.
pub(crate) struct ChecktimeCmd;

impl HostCmd<App> for ChecktimeCmd {
    fn name(&self) -> &'static str {
        "checktime"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.checktime_all();
        Some(ExEffect::Ok)
    }
}

/// `:vnew` — open a vertical split with a fresh empty unnamed buffer.
pub(crate) struct VnewCmd;

impl HostCmd<App> for VnewCmd {
    fn name(&self) -> &'static str {
        "vnew"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_vnew();
        Some(ExEffect::Ok)
    }
}

/// `:new` — open a horizontal split with a fresh empty unnamed buffer.
pub(crate) struct NewCmd;

impl HostCmd<App> for NewCmd {
    fn name(&self) -> &'static str {
        "new"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        3
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_new();
        Some(ExEffect::Ok)
    }
}

/// `:tabfirst` / `:tabrewind` / `:tabr` — jump to the first tab.
pub(crate) struct TabfirstCmd;

impl HostCmd<App> for TabfirstCmd {
    fn name(&self) -> &'static str {
        "tabfirst"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabrewind", "tabr"]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabfirst();
        Some(ExEffect::Ok)
    }
}

/// `:tablast` — jump to the last tab.
pub(crate) struct TablastCmd;

impl HostCmd<App> for TablastCmd {
    fn name(&self) -> &'static str {
        "tablast"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tablast();
        Some(ExEffect::Ok)
    }
}

// ── Phase 4f commands ────────────────────────────────────────────────────────

/// Parse a resize argument string (`"+N"`, `"-N"`, or `"N"`) into an i32 delta.
fn parse_resize_arg(arg: &str) -> Option<i32> {
    let arg = arg.trim();
    if arg.is_empty() {
        return None;
    }
    if let Some(rest) = arg.strip_prefix('+') {
        rest.trim().parse::<i32>().ok()
    } else if let Some(rest) = arg.strip_prefix('-') {
        rest.trim().parse::<i32>().ok().map(|n| -n)
    } else {
        arg.parse::<i32>().ok()
    }
}

/// `:tabonly` / `:tabo` — close all tabs except the current one.
pub(crate) struct TabonlyCmd;

impl HostCmd<App> for TabonlyCmd {
    fn name(&self) -> &'static str {
        "tabonly"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["tabo"]
    }

    /// Vim accepts `:tabo` (4 chars); match that minimum.
    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabonly();
        Some(ExEffect::Ok)
    }
}

/// `:tabs` — show an info popup listing all tabs.
pub(crate) struct TabsCmd;

impl HostCmd<App> for TabsCmd {
    fn name(&self) -> &'static str {
        "tabs"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Full name length — no standard abbreviation.
    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.do_tabs();
        Some(ExEffect::Ok)
    }
}

/// `:resize [+|-]N` — adjust focused window height.
pub(crate) struct ResizeCmd;

impl HostCmd<App> for ResizeCmd {
    fn name(&self) -> &'static str {
        "resize"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        6
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        if let Some(delta) = parse_resize_arg(args) {
            app.resize_height(delta);
            Some(ExEffect::Ok)
        } else {
            Some(ExEffect::Error("E: invalid resize argument".into()))
        }
    }
}

/// `:vertical resize [+|-]N` / `:vert res [+|-]N` — adjust focused window width.
///
/// `split_name_args` gives name=`"vertical"` (or `"vert"`), args=`"resize +5"` etc.
/// We strip the leading `resize`/`res` sub-word and parse the delta.
pub(crate) struct VerticalResizeCmd;

impl HostCmd<App> for VerticalResizeCmd {
    fn name(&self) -> &'static str {
        "vertical"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["vert"]
    }

    fn min_prefix(&self) -> usize {
        4
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        // args is e.g. "resize +5" or "res +5" or "resize"
        // Strip the mandatory "resize"/"res" sub-word; defer if absent.
        let rest = args
            .strip_prefix("resize")
            .or_else(|| args.strip_prefix("res"))?;
        if let Some(delta) = parse_resize_arg(rest) {
            app.resize_width(delta);
            Some(ExEffect::Ok)
        } else {
            Some(ExEffect::Error("E: invalid resize argument".into()))
        }
    }
}

/// `:Rename <newname>` — LSP symbol rename.
pub(crate) struct RenameCmd;

impl HostCmd<App> for RenameCmd {
    fn name(&self) -> &'static str {
        "Rename"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Full name — uppercase commands have no standard abbreviation.
    fn min_prefix(&self) -> usize {
        6
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        let new_name = args.trim().to_string();
        if new_name.is_empty() {
            // TODO: open prompt-based rename UI (Phase 6).
            Some(ExEffect::Error("E: usage: :Rename <newname>".into()))
        } else {
            app.lsp_rename(new_name);
            Some(ExEffect::Ok)
        }
    }
}

/// `:LspFormat` / `:Format` — LSP whole-file format.
pub(crate) struct LspFormatCmd;

impl HostCmd<App> for LspFormatCmd {
    fn name(&self) -> &'static str {
        "LspFormat"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["Format"]
    }

    fn min_prefix(&self) -> usize {
        9
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        // TODO: range formatting when invoked from visual mode (Phase 6).
        app.lsp_format();
        Some(ExEffect::Ok)
    }
}

/// `:GitDiff` — preview the git hunk under the cursor in an info popup (#115).
pub(crate) struct GitDiffCmd;

impl HostCmd<App> for GitDiffCmd {
    fn name(&self) -> &'static str {
        "GitDiff"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        "GitDiff".len()
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.git_show_hunk_diff();
        Some(ExEffect::Ok)
    }
}

/// `:GitStage` — stage the git hunk under the cursor into the index (#115).
pub(crate) struct GitStageCmd;

impl HostCmd<App> for GitStageCmd {
    fn name(&self) -> &'static str {
        "GitStage"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        "GitStage".len()
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.git_stage_hunk_at_cursor();
        Some(ExEffect::Ok)
    }
}

/// `:GitRevert` — discard the git hunk under the cursor, restoring HEAD (#115).
pub(crate) struct GitRevertCmd;

impl HostCmd<App> for GitRevertCmd {
    fn name(&self) -> &'static str {
        "GitRevert"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        "GitRevert".len()
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.git_revert_hunk_at_cursor();
        Some(ExEffect::Ok)
    }
}

/// `:LspCodeAction` / `:CodeAction` — LSP code actions.
pub(crate) struct LspCodeActionCmd;

impl HostCmd<App> for LspCodeActionCmd {
    fn name(&self) -> &'static str {
        "LspCodeAction"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["CodeAction"]
    }

    fn min_prefix(&self) -> usize {
        13
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.lsp_code_actions();
        Some(ExEffect::Ok)
    }
}

/// `:lopen` — open the diagnostics picker.
pub(crate) struct LopenCmd;

impl HostCmd<App> for LopenCmd {
    fn name(&self) -> &'static str {
        "lopen"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.open_diag_picker();
        Some(ExEffect::Ok)
    }
}

/// `:lnext` — jump to the next diagnostic.
pub(crate) struct LnextCmd;

impl HostCmd<App> for LnextCmd {
    fn name(&self) -> &'static str {
        "lnext"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.lnext_severity(None);
        Some(ExEffect::Ok)
    }
}

/// `:lprev` — jump to the previous diagnostic.
pub(crate) struct LprevCmd;

impl HostCmd<App> for LprevCmd {
    fn name(&self) -> &'static str {
        "lprev"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.lprev_severity(None);
        Some(ExEffect::Ok)
    }
}

/// `:lfirst` — jump to the first diagnostic.
pub(crate) struct LfirstCmd;

impl HostCmd<App> for LfirstCmd {
    fn name(&self) -> &'static str {
        "lfirst"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        6
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.ldiag_first();
        Some(ExEffect::Ok)
    }
}

/// `:llast` — jump to the last diagnostic.
pub(crate) struct LlastCmd;

impl HostCmd<App> for LlastCmd {
    fn name(&self) -> &'static str {
        "llast"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.ldiag_last();
        Some(ExEffect::Ok)
    }
}

/// `:LspInfo` — display LSP server status.
pub(crate) struct LspInfoCmd;

impl HostCmd<App> for LspInfoCmd {
    fn name(&self) -> &'static str {
        "LspInfo"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    fn min_prefix(&self) -> usize {
        7
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        app.show_lsp_info();
        Some(ExEffect::Ok)
    }
}

/// `:Anvil [install|uninstall|update] [name]` — plugin manager.
///
/// `split_name_args` gives name=`"Anvil"`, args=`"install foo"` etc.
pub(crate) struct AnvilCmd;

impl HostCmd<App> for AnvilCmd {
    fn name(&self) -> &'static str {
        "Anvil"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// Full name — no standard abbreviation.
    fn min_prefix(&self) -> usize {
        5
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        let parts: Vec<&str> = args.split_whitespace().collect();
        match parts.as_slice() {
            [] => {
                app.open_anvil_picker();
                Some(ExEffect::Ok)
            }
            ["install", name] => {
                app.anvil_install(name);
                Some(ExEffect::Ok)
            }
            ["uninstall", name] => {
                app.anvil_uninstall(name);
                Some(ExEffect::Ok)
            }
            ["update"] => {
                app.anvil_update_all();
                Some(ExEffect::Ok)
            }
            ["update", name] => {
                app.anvil_update(name);
                Some(ExEffect::Ok)
            }
            _ => Some(ExEffect::Error(
                "usage: :Anvil [install|uninstall|update] [name]".into(),
            )),
        }
    }
}

// ── Notifications command ──────────────────────────────────────────────────────

/// `:notifications` / `:notif` — dump the notification ring as an info popup.
///
/// Newest entry first. Format: `[-HH:MM:SS] [SEVERITY] body`.
pub(crate) struct NotificationsCmd;

impl HostCmd<App> for NotificationsCmd {
    fn name(&self) -> &'static str {
        "notifications"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["notif"]
    }

    fn min_prefix(&self) -> usize {
        5 // "notif"
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }

    fn run(&self, app: &mut App, _args: &str) -> Option<ExEffect> {
        use std::time::SystemTime;
        let now = SystemTime::now();
        let entries: Vec<String> = app
            .bus
            .history()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|h| {
                let elapsed = now
                    .duration_since(h.ts)
                    .map(|d| {
                        let secs = d.as_secs();
                        format!(
                            "-{:02}:{:02}:{:02}",
                            secs / 3600,
                            (secs % 3600) / 60,
                            secs % 60
                        )
                    })
                    .unwrap_or_else(|_| "?".to_string());
                format!(
                    "[{}] [{}] {}",
                    elapsed,
                    h.severity.label(),
                    h.display_body()
                )
            })
            .collect();
        let content = if entries.is_empty() {
            "(no notifications)".to_string()
        } else {
            entries.join("\n")
        };
        Some(ExEffect::Info(content))
    }
}

/// `:syntax [on|off|enable|disable|...]` — toggle bonsai syntax highlighting.
///
/// `on`/`enable` re-attaches grammars for every open slot's path and triggers
/// a fresh recompute. `off`/`disable` clears installed spans on every slot
/// and short-circuits future parses. Other args (e.g. `:syntax sync`,
/// `:syntax clear`) are accepted as no-ops for vim parity. Bare `:syntax`
/// reports current state.
pub(crate) struct SyntaxCmd;

impl HostCmd<App> for SyntaxCmd {
    fn name(&self) -> &'static str {
        "syntax"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }

    /// vim COMMAND_NAMES lists `:syntax` with abbrev `:syn`; match that.
    fn min_prefix(&self) -> usize {
        3
    }

    fn arg_kind(&self) -> ArgKind {
        ArgKind::Raw
    }

    fn run(&self, app: &mut App, args: &str) -> Option<ExEffect> {
        let arg = args.trim();
        if arg.is_empty() {
            let state = if app.syntax_enabled { "ON" } else { "OFF" };
            return Some(ExEffect::Info(format!("syntax: {state}")));
        }
        match arg.to_ascii_lowercase().as_str() {
            "on" | "enable" => {
                app.set_syntax_enabled(true);
                Some(ExEffect::Ok)
            }
            "off" | "disable" => {
                app.set_syntax_enabled(false);
                Some(ExEffect::Ok)
            }
            // Vim-permissive: accept other subcommands (`sync`, `clear`,
            // `reset`, language names, …) as no-ops rather than errors.
            _ => Some(ExEffect::Ok),
        }
    }
}

// ── Registry ─────────────────────────────────────────────────────────────────

fn build_registry() -> hjkl_ex::HostRegistry<App> {
    let mut reg = hjkl_ex::HostRegistry::new();
    // Phase 4a
    reg.add(Box::new(TabNextCmd));
    // Phase 4b
    reg.add(Box::new(SplitCmd));
    reg.add(Box::new(VsplitCmd));
    reg.add(Box::new(CloseCmd));
    reg.add(Box::new(TabnewCmd));
    reg.add(Box::new(TabprevCmd));
    reg.add(Box::new(TabcloseCmd));
    reg.add(Box::new(TabmoveCmd));
    reg.add(Box::new(OnlyCmd));
    // Phase 4c
    reg.add(Box::new(BnextCmd));
    reg.add(Box::new(BprevCmd));
    reg.add(Box::new(BfirstCmd));
    reg.add(Box::new(BlastCmd));
    reg.add(Box::new(BuffersCmd));
    reg.add(Box::new(ClipboardCmd));
    // Phase 4d2
    reg.add(Box::new(PickerCmd));
    reg.add(Box::new(RgCmd));
    reg.add(Box::new(BCmd));
    reg.add(Box::new(BpickerCmd));
    reg.add(Box::new(ChecktimeCmd));
    reg.add(Box::new(VnewCmd));
    reg.add(Box::new(NewCmd));
    reg.add(Box::new(TabfirstCmd));
    reg.add(Box::new(TablastCmd));
    // Phase 4f
    reg.add(Box::new(TabonlyCmd));
    reg.add(Box::new(TabsCmd));
    reg.add(Box::new(ResizeCmd));
    reg.add(Box::new(VerticalResizeCmd));
    reg.add(Box::new(RenameCmd));
    reg.add(Box::new(LspFormatCmd));
    reg.add(Box::new(GitDiffCmd));
    reg.add(Box::new(GitStageCmd));
    reg.add(Box::new(GitRevertCmd));
    reg.add(Box::new(LspCodeActionCmd));
    reg.add(Box::new(LopenCmd));
    reg.add(Box::new(LnextCmd));
    reg.add(Box::new(LprevCmd));
    reg.add(Box::new(LfirstCmd));
    reg.add(Box::new(LlastCmd));
    reg.add(Box::new(LspInfoCmd));
    reg.add(Box::new(AnvilCmd));
    reg.add(Box::new(NotificationsCmd));
    reg.add(Box::new(SyntaxCmd));
    reg
}

/// Static host registry — built once on first access, reused for every dispatch.
static HOST_REGISTRY: LazyLock<hjkl_ex::HostRegistry<App>> = LazyLock::new(build_registry);

/// Return a reference to the static host registry.
pub(crate) fn host_registry() -> &'static hjkl_ex::HostRegistry<App> {
    &HOST_REGISTRY
}
