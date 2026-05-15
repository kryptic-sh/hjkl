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

/// Build the host registry containing all Phase 4 app-level commands.
/// Subsequent phases extend this function.
///
/// Phase 4a note: the registry is constructed per-dispatch call (cheap at 1
/// command). Phase 4d-ish should cache via `std::sync::LazyLock` once command
/// count grows.
pub(crate) fn build_host_registry() -> hjkl_ex::HostRegistry<App> {
    let mut reg = hjkl_ex::HostRegistry::new();
    reg.add(Box::new(TabNextCmd));
    reg
}
