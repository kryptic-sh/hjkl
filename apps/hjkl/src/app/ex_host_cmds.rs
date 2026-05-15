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
    reg
}
