//! Phase 4a host-registry commands: app-level ex commands that need `&mut App`.
//!
//! Phase 4b–4e will add the bulk of app-level commands here.

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

/// Build the host registry containing all Phase 4 app-level commands.
/// Subsequent phases extend this function.
///
/// Phase 4a note: the registry is constructed per-dispatch call (cheap at 1
/// command). Phase 4d-ish should cache via `std::sync::LazyLock` once command
/// count grows.
pub(crate) fn build_host_registry() -> hjkl_ex::HostRegistry<App> {
    let mut reg = hjkl_ex::HostRegistry::new();
    reg.add(Box::new(TabNextCmd));
    reg.add(Box::new(SplitCmd));
    reg.add(Box::new(VsplitCmd));
    reg.add(Box::new(CloseCmd));
    reg.add(Box::new(TabnewCmd));
    reg.add(Box::new(TabprevCmd));
    reg.add(Box::new(TabcloseCmd));
    reg.add(Box::new(TabmoveCmd));
    reg.add(Box::new(OnlyCmd));
    // Phase 4c: buffer-nav commands.
    reg.add(Box::new(BnextCmd));
    reg.add(Box::new(BprevCmd));
    reg.add(Box::new(BfirstCmd));
    reg.add(Box::new(BlastCmd));
    reg.add(Box::new(BuffersCmd));
    reg.add(Box::new(ClipboardCmd));
    reg
}
