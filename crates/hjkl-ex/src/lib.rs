//! Ex-command registry and dispatch layer for the hjkl editor stack.
//!
//! Phase 1: provides an extensible [`Registry`] and a minimal set of
//! built-in commands (`:q`, `:q!`). Additional commands migrate in
//! subsequent phases.

pub use effect::ExEffect;
pub use registry::{ArgKind, ExCommand, Registry};

mod builtins;
mod effect;
mod parse;
mod registry;

/// Try to dispatch `input` (without the leading `:`) through the registry.
///
/// Returns:
/// - `Some(ExEffect)` when a registered command handled it
/// - `None` when no registered command matched (caller falls back to legacy handling)
pub fn try_dispatch<H: hjkl_engine::Host>(
    reg: &Registry<H>,
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: &str,
) -> Option<ExEffect> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (name, args) = parse::split_name_args(input);
    if name.is_empty() {
        return None;
    }
    let cmd = reg.resolve(name)?;
    Some((cmd.run)(editor, args))
}

/// Build a [`Registry`] seeded with the Phase 1 default commands (`:q`, `:q!`).
pub fn default_registry<H: hjkl_engine::Host>() -> Registry<H> {
    let mut reg = Registry::new();
    builtins::register_builtins(&mut reg);
    reg
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

    #[test]
    fn dispatch_q_returns_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "q");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_quit_returns_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "quit");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: false,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_q_bang_returns_force_quit() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "q!");
        assert_eq!(
            result,
            Some(ExEffect::Quit {
                force: true,
                save: false
            })
        );
    }

    #[test]
    fn dispatch_nonexistent_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "nonexistent");
        assert_eq!(result, None);
    }

    #[test]
    fn dispatch_empty_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "");
        assert_eq!(result, None);
    }

    #[test]
    fn dispatch_whitespace_only_returns_none() {
        let reg = default_registry::<DefaultHost>();
        let mut editor = make_editor();
        let result = try_dispatch(&reg, &mut editor, "   ");
        assert_eq!(result, None);
    }
}
