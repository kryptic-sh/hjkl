use crate::{
    effect::ExEffect,
    registry::{ArgKind, ExCommand, Registry},
};
use hjkl_engine::Host;

fn quit_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> ExEffect {
    ExEffect::Quit {
        force: false,
        save: false,
    }
}

fn quit_force_handler<H: Host>(
    _editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    _args: &str,
) -> ExEffect {
    ExEffect::Quit {
        force: true,
        save: false,
    }
}

/// Register the Phase 1 built-in commands: `:q` / `:quit` and `:q!` / `:quit!`.
pub(crate) fn register_builtins<H: Host>(reg: &mut Registry<H>) {
    reg.add(ExCommand {
        name: "quit",
        aliases: &[],
        arg_kind: ArgKind::None,
        min_prefix: 1,
        run: quit_handler::<H>,
    });
    reg.add(ExCommand {
        name: "quit!",
        aliases: &["q!"],
        arg_kind: ArgKind::None,
        // min_prefix for prefix match against "quit!" is moot since we register
        // "q!" as an explicit alias; set to 2 to match vim's :qu! style if needed.
        min_prefix: 2,
        run: quit_force_handler::<H>,
    });
}
