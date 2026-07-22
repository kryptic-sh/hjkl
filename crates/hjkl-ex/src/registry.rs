use crate::effect::ExEffect;
use hjkl_engine::Host;

/// The kinds of argument an ex command accepts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ArgKind {
    None,
    Path,
    View,
    Setting,
    Register,
    Mark,
    Colorscheme,
    Raw,
}

/// Type alias for a handler fn. Generic over `H` so a single `Registry<H>`
/// instance can serve any concrete host.
///
/// Returning `None` lets a handler opt out of a particular invocation — the
/// dispatcher then treats the command as unhandled and the umbrella's caller
/// falls back to its legacy ex path. This is how Phase 2a's bare-name
/// commands defer their `<path>`-arg variants to Phase 2b without a hard
/// `Unknown` error: e.g. `:w` returns `Some(Save)` but `:w foo` returns
/// `None` so `apps/hjkl::dispatch_ex` lets the legacy `SaveAs` arm handle it.
pub type ExHandler<H> = fn(
    &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    &str,
    Option<crate::range::LineRange>,
) -> Option<ExEffect>;

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

// ── Host-side registry ────────────────────────────────────────────────────────
//
// `HostCmd<Ctx>` is agnostic of the editor stack.  `apps/hjkl` supplies its
// `App` type as `Ctx`.  Commands here can mutate any application state, not
// just the editor.
//
// Range support is intentionally omitted for Phase 4 — the host-side commands
// migrating in 4b–4e (tab/window/picker/mapping ops) don't accept ranges.

/// Application-side ex command.  `Ctx` is opaque to hjkl-ex — `apps/hjkl`
/// supplies its `App` type.  Commands here can mutate any application state,
/// not just the editor.
pub trait HostCmd<Ctx>: Send + Sync {
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }
    fn min_prefix(&self) -> usize {
        1
    }
    fn arg_kind(&self) -> ArgKind {
        ArgKind::None
    }
    /// Returns `Some(effect)` to claim the invocation, `None` to defer.
    fn run(&self, ctx: &mut Ctx, args: &str) -> Option<crate::effect::ExEffect>;
}

/// Registry of host-level ex commands, generic over an opaque context type.
pub struct HostRegistry<Ctx> {
    cmds: Vec<Box<dyn HostCmd<Ctx>>>,
}

impl<Ctx> HostRegistry<Ctx> {
    pub fn new() -> Self {
        Self { cmds: Vec::new() }
    }

    /// Register a command.  Returns `&mut Self` for chaining.
    pub fn add(&mut self, cmd: Box<dyn HostCmd<Ctx>>) -> &mut Self {
        self.cmds.push(cmd);
        self
    }

    /// Resolve `name` to a registered host command.
    ///
    /// Priority:
    /// 1. Exact match against `cmd.name()`
    /// 2. Exact match against any alias in `cmd.aliases()`
    /// 3. Unambiguous prefix match against `cmd.name()` (input length >= `min_prefix()`)
    pub fn resolve(&self, name: &str) -> Option<&dyn HostCmd<Ctx>> {
        if name.is_empty() {
            return None;
        }
        // 1. Exact name match
        if let Some(c) = self.cmds.iter().find(|c| c.name() == name) {
            return Some(c.as_ref());
        }
        // 2. Exact alias match
        if let Some(c) = self.cmds.iter().find(|c| c.aliases().contains(&name)) {
            return Some(c.as_ref());
        }
        // 3. Unambiguous prefix match
        let candidates: Vec<&dyn HostCmd<Ctx>> = self
            .cmds
            .iter()
            .filter(|c| c.name().starts_with(name) && name.len() >= c.min_prefix())
            .map(|c| c.as_ref())
            .collect();
        if candidates.len() == 1 {
            Some(candidates[0])
        } else {
            None
        }
    }

    /// Iterate over all registered host commands.
    pub fn iter(&self) -> impl Iterator<Item = &dyn HostCmd<Ctx>> {
        self.cmds.iter().map(|c| c.as_ref())
    }
}

impl<Ctx> Default for HostRegistry<Ctx> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod host_registry_tests {
    use super::*;
    use crate::effect::ExEffect;

    struct TestCtx {
        value: i32,
    }

    struct IncrCmd;
    impl HostCmd<TestCtx> for IncrCmd {
        fn name(&self) -> &'static str {
            "increment"
        }
        fn aliases(&self) -> &'static [&'static str] {
            &["incr"]
        }
        fn min_prefix(&self) -> usize {
            3
        }
        fn run(&self, ctx: &mut TestCtx, _args: &str) -> Option<ExEffect> {
            ctx.value += 1;
            Some(ExEffect::Ok)
        }
    }

    struct ArgCmd;
    impl HostCmd<TestCtx> for ArgCmd {
        fn name(&self) -> &'static str {
            "argcmd"
        }
        fn min_prefix(&self) -> usize {
            6
        }
        fn run(&self, _ctx: &mut TestCtx, args: &str) -> Option<ExEffect> {
            if args.is_empty() {
                None
            } else {
                Some(ExEffect::Info(args.to_string()))
            }
        }
    }

    fn make_registry() -> HostRegistry<TestCtx> {
        let mut reg = HostRegistry::new();
        reg.add(Box::new(IncrCmd));
        reg.add(Box::new(ArgCmd));
        reg
    }

    #[test]
    fn resolve_exact_name() {
        let reg = make_registry();
        assert!(reg.resolve("increment").is_some());
        assert!(reg.resolve("argcmd").is_some());
    }

    #[test]
    fn resolve_exact_alias() {
        let reg = make_registry();
        assert!(reg.resolve("incr").is_some());
    }

    #[test]
    fn resolve_prefix() {
        let reg = make_registry();
        // "inc" meets min_prefix=3 for "increment" and is unambiguous
        assert!(reg.resolve("inc").is_some());
        assert_eq!(reg.resolve("inc").unwrap().name(), "increment");
    }

    #[test]
    fn resolve_prefix_too_short() {
        let reg = make_registry();
        // "in" is shorter than min_prefix=3 for "increment"
        assert!(reg.resolve("in").is_none());
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let reg = make_registry();
        assert!(reg.resolve("nonexistent").is_none());
        assert!(reg.resolve("").is_none());
    }

    #[test]
    fn run_mutates_context() {
        let reg = make_registry();
        let mut ctx = TestCtx { value: 0 };
        let cmd = reg.resolve("increment").unwrap();
        let eff = cmd.run(&mut ctx, "");
        assert_eq!(eff, Some(ExEffect::Ok));
        assert_eq!(ctx.value, 1);
    }

    #[test]
    fn run_returns_none_to_defer() {
        let reg = make_registry();
        let mut ctx = TestCtx { value: 0 };
        let cmd = reg.resolve("argcmd").unwrap();
        // no args → defers
        let eff = cmd.run(&mut ctx, "");
        assert!(eff.is_none());
        // with args → claims
        let eff2 = cmd.run(&mut ctx, "hello");
        assert_eq!(eff2, Some(ExEffect::Info("hello".to_string())));
    }

    #[test]
    fn iter_yields_all_commands() {
        let reg = make_registry();
        let names: Vec<&str> = reg.iter().map(|c| c.name()).collect();
        assert!(names.contains(&"increment"));
        assert!(names.contains(&"argcmd"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::ExEffect;
    use hjkl_engine::{DefaultHost, Editor};

    fn noop_handler(
        _editor: &mut Editor<hjkl_buffer::View, DefaultHost>,
        _args: &str,
        _range: Option<crate::range::LineRange>,
    ) -> Option<ExEffect> {
        Some(ExEffect::Ok)
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
