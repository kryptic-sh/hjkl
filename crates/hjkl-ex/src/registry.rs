use crate::effect::ExEffect;
use hjkl_engine::Host;

/// The kinds of argument an ex command accepts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ArgKind {
    None,
    Path,
    Buffer,
    Setting,
    Register,
    Mark,
    Raw,
}

/// Type alias for a handler fn. Generic over `H` so a single `Registry<H>`
/// instance can serve any concrete host.
pub type ExHandler<H> = fn(&mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>, &str) -> ExEffect;

/// A single registered ex command.
pub struct ExCommand<H: Host> {
    /// Canonical full name (e.g. `"quit"`).
    pub name: &'static str,
    /// Alternate full names (e.g. `["quit!"]`).
    pub aliases: &'static [&'static str],
    /// What kind of argument this command takes.
    pub arg_kind: ArgKind,
    /// Minimum prefix length for vim-style abbreviation (1 → `:q` resolves to `:quit`).
    pub min_prefix: usize,
    /// Handler called with the editor and args (text after the command name, trimmed).
    pub run: ExHandler<H>,
}

/// Registry of ex commands for a concrete host type `H`.
pub struct Registry<H: Host> {
    cmds: Vec<ExCommand<H>>,
}

impl<H: Host> Registry<H> {
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }

    /// Register a command. Returns `&mut Self` for chaining.
    pub fn add(&mut self, cmd: ExCommand<H>) -> &mut Self {
        self.cmds.push(cmd);
        self
    }

    /// Resolve `name` to a registered command.
    ///
    /// Priority:
    /// 1. Exact match against `cmd.name`
    /// 2. Exact match against any alias in `cmd.aliases`
    /// 3. Unambiguous prefix match against `cmd.name` (input length >= `min_prefix`)
    pub fn resolve(&self, name: &str) -> Option<&ExCommand<H>> {
        if name.is_empty() {
            return None;
        }
        // 1. Exact name match
        if let Some(cmd) = self.cmds.iter().find(|c| c.name == name) {
            return Some(cmd);
        }
        // 2. Exact alias match
        if let Some(cmd) = self.cmds.iter().find(|c| c.aliases.contains(&name)) {
            return Some(cmd);
        }
        // 3. Prefix match
        let candidates: Vec<&ExCommand<H>> = self
            .cmds
            .iter()
            .filter(|c| c.name.starts_with(name) && name.len() >= c.min_prefix)
            .collect();
        if candidates.len() == 1 {
            Some(candidates[0])
        } else {
            None
        }
    }

    /// Iterate over all registered commands.
    pub fn iter(&self) -> impl Iterator<Item = &ExCommand<H>> {
        self.cmds.iter()
    }
}

impl<H: Host> Default for Registry<H> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ExEffect;
    use hjkl_engine::{DefaultHost, Editor};

    fn noop_handler(
        _editor: &mut Editor<hjkl_buffer::Buffer, DefaultHost>,
        _args: &str,
    ) -> ExEffect {
        ExEffect::Ok
    }

    fn make_registry() -> Registry<DefaultHost> {
        let mut reg = Registry::new();
        reg.add(ExCommand {
            name: "quit",
            aliases: &["quit!"],
            arg_kind: ArgKind::None,
            min_prefix: 1,
            run: noop_handler,
        });
        reg.add(ExCommand {
            name: "write",
            aliases: &[],
            arg_kind: ArgKind::Path,
            min_prefix: 1,
            run: noop_handler,
        });
        reg
    }

    #[test]
    fn resolve_exact_name() {
        let reg = make_registry();
        assert!(reg.resolve("quit").is_some());
        assert!(reg.resolve("write").is_some());
    }

    #[test]
    fn resolve_exact_alias() {
        let reg = make_registry();
        assert!(reg.resolve("quit!").is_some());
    }

    #[test]
    fn resolve_prefix() {
        let reg = make_registry();
        // "q" is a valid prefix (min_prefix=1) and unambiguous among registered cmds
        assert!(reg.resolve("q").is_some());
        assert!(reg.resolve("w").is_some());
    }

    #[test]
    fn resolve_prefix_too_short() {
        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "quit",
            aliases: &[],
            arg_kind: ArgKind::None,
            min_prefix: 2,
            run: noop_handler,
        });
        // "q" is shorter than min_prefix=2, should not resolve
        assert!(reg.resolve("q").is_none());
        // "qu" meets min_prefix=2
        assert!(reg.resolve("qu").is_some());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let reg = make_registry();
        assert!(reg.resolve("nonexistent").is_none());
        assert!(reg.resolve("").is_none());
    }

    #[test]
    fn add_command_works() {
        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "test",
            aliases: &[],
            arg_kind: ArgKind::Raw,
            min_prefix: 2,
            run: noop_handler,
        });
        assert!(reg.resolve("test").is_some());
        assert!(reg.resolve("te").is_some());
        assert!(reg.resolve("t").is_none());
    }
}
