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

/// `:perf` — toggle perf overlay and reset counters.
pub(crate) struct PerfCmd;

impl HostCmd<App> for PerfCmd {
    fn name(&self) -> &'static str {
        "perf"
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
        app.perf_overlay = !app.perf_overlay;
        app.recompute_hits = 0;
        app.recompute_throttled = 0;
        app.recompute_runs = 0;
        app.status_message = Some(if app.perf_overlay {
            "perf overlay: on (counters reset)".into()
        } else {
            "perf overlay: off".into()
        });
        Some(ExEffect::Ok)
    }
}

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
    reg.add(Box::new(PerfCmd));
    reg.add(Box::new(PickerCmd));
    reg.add(Box::new(RgCmd));
    reg.add(Box::new(BCmd));
    reg.add(Box::new(BpickerCmd));
    reg.add(Box::new(ChecktimeCmd));
    reg.add(Box::new(VnewCmd));
    reg.add(Box::new(NewCmd));
    reg.add(Box::new(TabfirstCmd));
    reg.add(Box::new(TablastCmd));
    reg
}

/// Static host registry — built once on first access, reused for every dispatch.
static HOST_REGISTRY: LazyLock<hjkl_ex::HostRegistry<App>> = LazyLock::new(build_registry);

/// Return a reference to the static host registry.
pub(crate) fn host_registry() -> &'static hjkl_ex::HostRegistry<App> {
    &HOST_REGISTRY
}
