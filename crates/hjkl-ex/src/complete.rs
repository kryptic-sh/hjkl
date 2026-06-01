use std::ops::Range;

use crate::{ArgKind, HostRegistry, Registry};

/// What kind of token is being completed. Phase 5a only emits `Command`;
/// Phase 6 adds Path/Setting/Buffer/Register/Mark for arg completion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    None,
    Command,
    Path,
    Setting,
    Buffer,
    Register,
    Mark,
}

/// Sources for arg completion. Caller fills the slots applicable to
/// their context. None means "no candidates" — completer returns empty.
#[derive(Default)]
pub struct ArgSources<'a> {
    /// cwd to scan for `:e <Tab>` style path completion. None disables.
    pub cwd: Option<&'a std::path::Path>,
    /// All known option names + aliases for `:set <Tab>`. Empty disables.
    pub settings: &'a [String],
    /// Open buffer names for `:b <Tab>`. Empty disables.
    pub buffers: &'a [String],
    /// Non-empty register selectors (e.g. `"a"`, `"+"`, `"0"`) for `:reg`/`:put`.
    pub registers: &'a [String],
    /// Live mark names for `:marks`/`:delmarks`. Empty disables.
    pub marks: &'a [String],
}

/// Completion candidates for an input line at a given caret offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completions {
    /// Byte range in the original input that should be replaced when a
    /// candidate is accepted. For command completion this is the leading
    /// command-name token range.
    pub replace_range: Range<usize>,
    /// Candidate strings sorted alphabetically. Empty when nothing matches.
    pub candidates: Vec<String>,
    /// Token kind. `None` when caret is outside any completable position.
    pub kind: CompletionKind,
}

impl Completions {
    /// Empty completion at caret position — kind=None, no candidates.
    pub fn empty(caret: usize) -> Self {
        Self {
            replace_range: caret..caret,
            candidates: Vec::new(),
            kind: CompletionKind::None,
        }
    }
}

/// Compute the longest common prefix of a non-empty slice of strings.
/// Returns "" when the slice is empty or the LCP is empty.
pub fn longest_common_prefix(candidates: &[String]) -> String {
    if candidates.is_empty() {
        return String::new();
    }
    let first = &candidates[0];
    let mut end = first.len();
    for s in &candidates[1..] {
        end = end.min(s.len());
        end = first
            .as_bytes()
            .iter()
            .zip(s.as_bytes().iter())
            .take(end)
            .take_while(|(a, b)| a == b)
            .count();
        if end == 0 {
            return String::new();
        }
    }
    first[..end].to_string()
}

/// Complete a partial command name at the given caret position from a flat
/// list of candidate names. The line may contain a leading range prefix
/// (`5,10`, `%`, etc.) — those are NOT consumed here; the caller must pass
/// the substring after any range. Phase 6 generalizes this for arg completion.
///
/// Returns:
/// - `kind: Command` when the caret sits inside a leading alpha-prefix token
/// - `kind: None` otherwise (e.g. caret past the first whitespace, line empty)
///
/// Candidates: every name from `available` that has the typed prefix as its
/// own prefix, sorted alphabetically. Includes both canonical names and
/// aliases — caller decides what to merge in.
pub fn complete_command_from_names(line: &str, caret: usize, available: &[String]) -> Completions {
    let caret = caret.min(line.len());
    if line.is_empty() {
        return Completions {
            replace_range: 0..0,
            candidates: available.to_vec(),
            kind: CompletionKind::Command,
        };
    }
    // Identify the command-name token: leading run of ASCII alpha + optional
    // trailing `!`, but only if caret is inside that span.
    let alpha_end = line
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let token_end = if line.as_bytes().get(alpha_end) == Some(&b'!') {
        alpha_end + 1
    } else {
        alpha_end
    };
    if caret > token_end {
        return Completions::empty(caret);
    }
    let prefix = &line[..caret];
    let mut candidates: Vec<String> = available
        .iter()
        .filter(|n| n.starts_with(prefix))
        .cloned()
        .collect();
    candidates.sort();
    candidates.dedup();
    Completions {
        replace_range: 0..token_end,
        candidates,
        kind: CompletionKind::Command,
    }
}

/// Collect command-name candidates (canonical name + aliases) from a Registry.
pub fn collect_registry_names<H: hjkl_engine::Host>(reg: &Registry<H>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for cmd in reg.iter() {
        names.push(cmd.name.to_string());
        names.extend(cmd.aliases.iter().map(|a| a.to_string()));
    }
    names
}

/// Same for HostRegistry.
pub fn collect_host_registry_names<Ctx>(reg: &HostRegistry<Ctx>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for cmd in reg.iter() {
        names.push(cmd.name().to_string());
        names.extend(cmd.aliases().iter().map(|a| a.to_string()));
    }
    names
}

// ── Arg-position helpers ──────────────────────────────────────────────────────

/// Returns `(end_byte_offset_of_command_token, did_find_space_after)`.
///
/// The command token is the leading run of ASCII alpha characters with an
/// optional trailing `!`. We don't consume the space itself.
pub fn first_word_end(line: &str) -> (usize, bool) {
    let alpha_end = line
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphabetic())
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let token_end = if line.as_bytes().get(alpha_end) == Some(&b'!') {
        alpha_end + 1
    } else {
        alpha_end
    };
    let has_space = line.as_bytes().get(token_end) == Some(&b' ');
    (token_end, has_space)
}

/// Scan `cwd` for entries whose names begin with `file_part` (respecting the
/// `dir_part` prefix).  Appends `/` to directories.  Hidden entries (starting
/// with `.`) are skipped unless `file_part` itself starts with `.`.
fn complete_path_entries(prefix: &str, cwd: &std::path::Path) -> Vec<String> {
    // Split prefix at the last '/' into (dir_part, file_part).
    let (dir_part, file_part) = match prefix.rfind('/') {
        Some(idx) => (&prefix[..=idx], &prefix[idx + 1..]),
        None => ("", prefix),
    };
    let scan_dir = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else if std::path::Path::new(dir_part).is_absolute() {
        std::path::PathBuf::from(dir_part)
    } else {
        cwd.join(dir_part)
    };
    let rd = match std::fs::read_dir(&scan_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };
    let show_hidden = file_part.starts_with('.');
    let mut results: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let name_str = name.to_str()?.to_string();
            // Skip hidden unless file_part starts with '.'
            if !show_hidden && name_str.starts_with('.') {
                return None;
            }
            if !name_str.starts_with(file_part) {
                return None;
            }
            let suffix = if e.file_type().ok()?.is_dir() {
                "/"
            } else {
                ""
            };
            Some(format!("{dir_part}{name_str}{suffix}"))
        })
        .collect();
    results.sort();
    results
}

/// Short human label for the argument a command takes, for completion docs.
/// `ArgKind::None` → "" (command takes no argument).
pub fn arg_kind_usage(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::None => "",
        ArgKind::Path => "<path>",
        ArgKind::Buffer => "<buffer>",
        ArgKind::Setting => "<setting>",
        ArgKind::Register => "<register>",
        ArgKind::Mark => "<mark>",
        ArgKind::Raw => "<args>",
    }
}

/// A command-completion candidate enriched with metadata for the UI.
/// `name` is the canonical command text to insert. `arg_kind` is the kind of
/// argument the resolved command accepts (`ArgKind::None` when it takes none).
/// `takes_arg` is `arg_kind != ArgKind::None` — the UI appends a trailing space
/// on accept only when this is true. `usage` is `arg_kind_usage(arg_kind)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCandidate {
    pub name: String,
    pub arg_kind: ArgKind,
    pub takes_arg: bool,
    pub usage: &'static str,
}

/// Like [`complete`], but for the COMMAND-NAME position it returns enriched
/// [`CommandCandidate`]s (name + arg metadata) instead of bare strings. Returns
/// `(replace_range, Vec<CommandCandidate>)`. For ARG positions (caret past the
/// command name) it returns an EMPTY vec with the arg-token replace_range — arg
/// candidates have no command metadata, so callers use plain `complete()` for
/// those. Use this for command-name docs; fall back to `complete()` for args.
pub fn complete_command_meta<H, Ctx>(
    line: &str,
    caret: usize,
    editor_reg: &Registry<H>,
    host_reg: &HostRegistry<Ctx>,
) -> (std::ops::Range<usize>, Vec<CommandCandidate>)
where
    H: hjkl_engine::Host,
{
    let caret = caret.min(line.len());
    let (cmd_token_end, _) = first_word_end(line);
    // Caret is past the command-name token → arg position, no command meta.
    if caret > cmd_token_end {
        return (caret..caret, vec![]);
    }
    // Gather all candidate names, same as the command-name path in complete().
    let mut names = collect_host_registry_names(host_reg);
    names.extend(collect_registry_names(editor_reg));
    names.sort();
    names.dedup();
    // Filter to those that start with the typed prefix.
    let prefix = &line[..caret];
    let mut names: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .collect();
    names.sort();
    names.dedup();
    // Build enriched candidates.
    let candidates: Vec<CommandCandidate> = names
        .into_iter()
        .map(|name| {
            let arg_kind = host_reg
                .resolve(&name)
                .map(|c| c.arg_kind())
                .or_else(|| editor_reg.resolve(&name).map(|c| c.arg_kind))
                .unwrap_or(ArgKind::None);
            let takes_arg = arg_kind != ArgKind::None;
            let usage = arg_kind_usage(arg_kind);
            CommandCandidate {
                name,
                arg_kind,
                takes_arg,
                usage,
            }
        })
        .collect();
    (0..cmd_token_end, candidates)
}

/// Per-arg-kind completion. Caller resolves the command and passes its
/// arg_kind. Returns empty Completions when caret isn't in arg position,
/// or when no sources match.
pub fn complete_arg(
    line: &str,
    caret: usize,
    arg_kind: ArgKind,
    sources: &ArgSources<'_>,
) -> Completions {
    let caret = caret.min(line.len());
    // Find end of command token.
    let (cmd_end, has_space) = first_word_end(line);
    // Arg position starts at cmd_end + 1 (past the space).
    let arg_start = if has_space { cmd_end + 1 } else { cmd_end };
    if caret <= cmd_end || !has_space {
        // Caret still in command-name territory.
        return Completions::empty(caret);
    }
    // Find token under caret: walk back from caret to previous whitespace.
    let slice = &line[arg_start..caret];
    let token_offset = slice
        .rfind(|c: char| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    let token_start = arg_start + token_offset;
    let prefix = &line[token_start..caret];

    let (candidates, kind) = match arg_kind {
        ArgKind::None | ArgKind::Raw => return Completions::empty(caret),
        ArgKind::Path => {
            let cwd = match sources.cwd {
                Some(p) => p,
                None => return Completions::empty(caret),
            };
            (complete_path_entries(prefix, cwd), CompletionKind::Path)
        }
        ArgKind::Setting => {
            let mut c: Vec<String> = sources
                .settings
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::Setting)
        }
        ArgKind::Buffer => {
            let mut c: Vec<String> = sources
                .buffers
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::Buffer)
        }
        ArgKind::Register => {
            let mut c: Vec<String> = sources
                .registers
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::Register)
        }
        ArgKind::Mark => {
            let mut c: Vec<String> = sources
                .marks
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::Mark)
        }
    };

    Completions {
        replace_range: token_start..caret,
        candidates,
        kind,
    }
}

/// High-level orchestrator: resolve the command name in `line` against both
/// registries, then dispatch to arg completer or command-name completer.
///
/// Falls back to Phase 5a's command completer when caret is in command-name
/// position.
pub fn complete<H, Ctx>(
    line: &str,
    caret: usize,
    editor_reg: &Registry<H>,
    host_reg: &HostRegistry<Ctx>,
    sources: &ArgSources<'_>,
) -> Completions
where
    H: hjkl_engine::Host,
{
    let (cmd_token_end, has_arg_space) = first_word_end(line);
    let caret = caret.min(line.len());
    if caret <= cmd_token_end {
        // Command-name completion path.
        let mut names = collect_host_registry_names(host_reg);
        names.extend(collect_registry_names(editor_reg));
        names.sort();
        names.dedup();
        return complete_command_from_names(line, caret, &names);
    }
    if !has_arg_space {
        return Completions::empty(caret);
    }
    // Arg position — resolve command name to find arg_kind.
    let cmd_name = &line[..cmd_token_end];
    let arg_kind = host_reg
        .resolve(cmd_name)
        .map(|c| c.arg_kind())
        .or_else(|| editor_reg.resolve(cmd_name).map(|c| c.arg_kind))
        .unwrap_or(ArgKind::None);
    complete_arg(line, caret, arg_kind, sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn complete_empty_line_returns_all_names() {
        let available = names(&["quit", "write"]);
        let result = complete_command_from_names("", 0, &available);
        assert_eq!(result.kind, CompletionKind::Command);
        assert_eq!(result.replace_range, 0..0);
        assert!(result.candidates.contains(&"quit".to_string()));
        assert!(result.candidates.contains(&"write".to_string()));
    }

    #[test]
    fn complete_q_returns_quit() {
        let available = names(&["quit", "write"]);
        let result = complete_command_from_names("q", 1, &available);
        assert_eq!(result.kind, CompletionKind::Command);
        assert_eq!(result.replace_range, 0..1);
        assert_eq!(result.candidates, vec!["quit".to_string()]);
    }

    #[test]
    fn complete_w_returns_two_names() {
        let available = names(&["wall", "write"]);
        let result = complete_command_from_names("w", 1, &available);
        assert_eq!(result.kind, CompletionKind::Command);
        assert_eq!(result.replace_range, 0..1);
        assert_eq!(
            result.candidates,
            vec!["wall".to_string(), "write".to_string()]
        );
    }

    #[test]
    fn complete_caret_past_alpha_returns_none() {
        let available = names(&["quit", "write"]);
        let result = complete_command_from_names("q ", 2, &available);
        assert_eq!(result.kind, CompletionKind::None);
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn complete_dedup_aliases() {
        let available = names(&["quit", "quit", "write"]);
        let result = complete_command_from_names("q", 1, &available);
        assert_eq!(result.candidates, vec!["quit".to_string()]);
    }

    #[test]
    fn complete_with_bang() {
        let available = names(&["quit", "quit!", "qall"]);
        let result = complete_command_from_names("q", 1, &available);
        assert_eq!(result.kind, CompletionKind::Command);
        // All three start with "q"
        assert!(result.candidates.contains(&"quit".to_string()));
        assert!(result.candidates.contains(&"quit!".to_string()));
        assert!(result.candidates.contains(&"qall".to_string()));
    }

    #[test]
    fn lcp_empty() {
        assert_eq!(longest_common_prefix(&[]), "");
    }

    #[test]
    fn lcp_single() {
        assert_eq!(longest_common_prefix(&["quit".to_string()]), "quit");
    }

    #[test]
    fn lcp_common() {
        let candidates = names(&["wall", "write", "wq"]);
        assert_eq!(longest_common_prefix(&candidates), "w");
    }

    #[test]
    fn lcp_no_common() {
        let candidates = names(&["a", "b"]);
        assert_eq!(longest_common_prefix(&candidates), "");
    }

    // ── Phase 6 tests ─────────────────────────────────────────────────────────

    fn str_vec(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn arg_position_detection_with_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        // Write a file so read_dir has at least one result.
        std::fs::write(tmp.path().join("foo.txt"), b"x").unwrap();
        let sources = ArgSources {
            cwd: Some(tmp.path()),
            ..Default::default()
        };
        // "e " caret=2 → arg position, path completion → non-empty
        let result = complete_arg("e ", 2, ArgKind::Path, &sources);
        assert_eq!(result.kind, CompletionKind::Path);
        assert!(!result.candidates.is_empty());
        assert!(result.candidates.iter().any(|c| c.contains("foo.txt")));
    }

    #[test]
    fn complete_set_filters_settings() {
        let settings = str_vec(&["number", "numberwidth", "nu", "noic", "relativenumber"]);
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        let result = complete_arg("set ", 4, ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::Setting);
        // prefix "" → all settings
        assert!(result.candidates.contains(&"number".to_string()));
        assert!(result.candidates.contains(&"numberwidth".to_string()));
        assert!(result.candidates.contains(&"nu".to_string()));

        // Now filter with prefix "nu"
        let result2 = complete_arg("set nu", 6, ArgKind::Setting, &sources);
        assert_eq!(result2.kind, CompletionKind::Setting);
        assert!(result2.candidates.contains(&"number".to_string()));
        assert!(result2.candidates.contains(&"numberwidth".to_string()));
        assert!(result2.candidates.contains(&"nu".to_string()));
        assert!(!result2.candidates.contains(&"noic".to_string()));
        assert!(!result2.candidates.contains(&"relativenumber".to_string()));
    }

    #[test]
    fn complete_buffer_filters_buffers() {
        let buffers = str_vec(&["src/main.rs", "src/lib.rs", "tests/foo.rs"]);
        let sources = ArgSources {
            buffers: &buffers,
            ..Default::default()
        };
        let result = complete_arg("b ", 2, ArgKind::Buffer, &sources);
        assert_eq!(result.kind, CompletionKind::Buffer);
        assert!(result.candidates.contains(&"src/main.rs".to_string()));
        assert!(result.candidates.contains(&"src/lib.rs".to_string()));
        assert!(result.candidates.contains(&"tests/foo.rs".to_string()));

        let result2 = complete_arg("b src", 5, ArgKind::Buffer, &sources);
        assert_eq!(result2.kind, CompletionKind::Buffer);
        assert!(result2.candidates.contains(&"src/main.rs".to_string()));
        assert!(result2.candidates.contains(&"src/lib.rs".to_string()));
        assert!(!result2.candidates.contains(&"tests/foo.rs".to_string()));
    }

    #[test]
    fn complete_register_filters() {
        let regs = str_vec(&["\"\"", "\"0", "\"a", "\"b"]);
        let sources = ArgSources {
            registers: &regs,
            ..Default::default()
        };
        let result = complete_arg("reg ", 4, ArgKind::Register, &sources);
        assert_eq!(result.kind, CompletionKind::Register);
        assert!(result.candidates.contains(&"\"a".to_string()));

        // prefix "\"a" → only "\"a"
        let result2 = complete_arg("reg \"a", 6, ArgKind::Register, &sources);
        assert!(result2.candidates.contains(&"\"a".to_string()));
        assert!(!result2.candidates.contains(&"\"b".to_string()));
    }

    #[test]
    fn complete_mark_filters() {
        let marks = str_vec(&["a", "b", "c"]);
        let sources = ArgSources {
            marks: &marks,
            ..Default::default()
        };
        // prefix "" → all marks
        let result = complete_arg("marks ", 6, ArgKind::Mark, &sources);
        assert_eq!(result.kind, CompletionKind::Mark);
        assert_eq!(result.candidates.len(), 3);

        // prefix "a" → only "a"
        let result2 = complete_arg("marks a", 7, ArgKind::Mark, &sources);
        assert_eq!(result2.candidates, vec!["a".to_string()]);
    }

    #[test]
    fn complete_path_skips_hidden_unless_dot() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".hidden"), b"x").unwrap();
        std::fs::write(tmp.path().join("visible.txt"), b"x").unwrap();

        let sources = ArgSources {
            cwd: Some(tmp.path()),
            ..Default::default()
        };

        // prefix "" → hidden skipped
        let result = complete_arg("e ", 2, ArgKind::Path, &sources);
        assert!(result.candidates.iter().all(|c| !c.starts_with(".hidden")));
        assert!(result.candidates.iter().any(|c| c.contains("visible.txt")));

        // prefix "." → hidden shown
        let result2 = complete_arg("e .", 3, ArgKind::Path, &sources);
        assert!(result2.candidates.iter().any(|c| c.contains(".hidden")));
    }

    #[test]
    fn complete_in_command_position_falls_back_to_command() {
        use crate::{ExCommand, Registry};
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, DefaultHost>,
            _: &str,
            _: Option<crate::range::LineRange>,
        ) -> Option<crate::effect::ExEffect> {
            None
        }

        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "edit",
            aliases: &["e"],
            arg_kind: ArgKind::Path,
            min_prefix: 1,
            run: noop,
        });
        let host_reg = HostRegistry::<()>::new();
        let sources = ArgSources::default();

        // caret=1, line="e" → command position
        let result = complete("e", 1, &reg, &host_reg, &sources);
        assert_eq!(result.kind, CompletionKind::Command);
    }

    #[test]
    fn complete_unknown_command_returns_none_kind() {
        use crate::Registry;
        use hjkl_engine::DefaultHost;

        let reg = Registry::<DefaultHost>::new();
        let host_reg = HostRegistry::<()>::new();
        let sources = ArgSources::default();

        // "xxx " with unknown command → kind=None
        let result = complete("xxx ", 4, &reg, &host_reg, &sources);
        assert_eq!(result.kind, CompletionKind::None);
        assert!(result.candidates.is_empty());
    }

    // ── complete_command_meta tests ───────────────────────────────────────────

    #[test]
    fn arg_kind_usage_labels() {
        assert_eq!(arg_kind_usage(ArgKind::None), "");
        assert_eq!(arg_kind_usage(ArgKind::Path), "<path>");
        assert_eq!(arg_kind_usage(ArgKind::Buffer), "<buffer>");
        assert_eq!(arg_kind_usage(ArgKind::Setting), "<setting>");
        assert_eq!(arg_kind_usage(ArgKind::Register), "<register>");
        assert_eq!(arg_kind_usage(ArgKind::Mark), "<mark>");
        assert_eq!(arg_kind_usage(ArgKind::Raw), "<args>");
    }

    #[test]
    fn complete_command_meta_returns_arg_kinds() {
        use crate::{ExCommand, Registry};
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, DefaultHost>,
            _: &str,
            _: Option<crate::range::LineRange>,
        ) -> Option<crate::effect::ExEffect> {
            None
        }

        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "quit",
            aliases: &[],
            arg_kind: ArgKind::None,
            min_prefix: 1,
            run: noop,
        });
        reg.add(ExCommand {
            name: "edit",
            aliases: &["e"],
            arg_kind: ArgKind::Path,
            min_prefix: 1,
            run: noop,
        });
        let host_reg = HostRegistry::<()>::new();

        // "e" at caret=1 → command position; matches both "e" (alias) and "edit"
        let (range, candidates) = complete_command_meta("e", 1, &reg, &host_reg);
        assert_eq!(range, 0..1);

        let edit_cand = candidates.iter().find(|c| c.name == "edit");
        assert!(
            edit_cand.is_some(),
            "expected 'edit' in candidates: {candidates:?}"
        );
        let edit_cand = edit_cand.unwrap();
        assert_eq!(edit_cand.arg_kind, ArgKind::Path);
        assert!(edit_cand.takes_arg);
        assert_eq!(edit_cand.usage, "<path>");

        // "quit" doesn't start with "e", but verify a None-arg command via full match
        let (_, all_candidates) = complete_command_meta("quit", 4, &reg, &host_reg);
        let quit_cand = all_candidates.iter().find(|c| c.name == "quit");
        assert!(
            quit_cand.is_some(),
            "expected 'quit' in candidates: {all_candidates:?}"
        );
        let quit_cand = quit_cand.unwrap();
        assert_eq!(quit_cand.arg_kind, ArgKind::None);
        assert!(!quit_cand.takes_arg);
        assert_eq!(quit_cand.usage, "");
    }

    #[test]
    fn complete_command_meta_arg_position_is_empty() {
        use crate::{ExCommand, Registry};
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, DefaultHost>,
            _: &str,
            _: Option<crate::range::LineRange>,
        ) -> Option<crate::effect::ExEffect> {
            None
        }

        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "edit",
            aliases: &["e"],
            arg_kind: ArgKind::Path,
            min_prefix: 1,
            run: noop,
        });
        let host_reg = HostRegistry::<()>::new();

        // "edit " with caret=5 → arg position (past the command name + space)
        let (_, candidates) = complete_command_meta("edit ", 5, &reg, &host_reg);
        assert!(
            candidates.is_empty(),
            "expected empty candidates for arg position, got: {candidates:?}"
        );
    }
}
