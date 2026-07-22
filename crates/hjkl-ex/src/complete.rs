use std::ops::Range;

use crate::{ArgKind, HostRegistry, Registry};

/// What kind of token is being completed. Phase 5a only emits `Command`;
/// Phase 6 adds Path/Setting/View/Register/Mark for arg completion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    None,
    Command,
    Path,
    Setting,
    /// A value for a `name=value` `:set` option (e.g. the `dark` in
    /// `background=dark`). Distinct from `Setting` so the UI can label it.
    SettingValue,
    View,
    Register,
    Mark,
    Colorscheme,
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
    /// Bundled colorscheme names for `:colorscheme <Tab>`. Empty disables.
    pub colorschemes: &'a [String],
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
    // `end` counts matching BYTES; back off to a char boundary so candidates
    // sharing only a partial multibyte prefix (e.g. "à" C3A0 vs "á" C3A1
    // match on the C3 byte) don't slice mid-char and panic.
    while !first.is_char_boundary(end) {
        end -= 1;
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
        // Sort + dedup like the non-empty path so direct callers get the
        // documented alphabetical order regardless of `available`'s order.
        let mut candidates = available.to_vec();
        candidates.sort();
        candidates.dedup();
        return Completions {
            replace_range: 0..0,
            candidates,
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

/// Byte length of a leading ex RANGE/count prefix (`%`, `5,10`, `.,$`, `2`,
/// `'a,'b`, `/pat/`, `?pat?`, `+3`, …). The command name begins at the returned
/// offset. This is a lightweight scanner — it does NOT resolve addresses or
/// validate the range (that needs an editor); it only lets command-NAME
/// completion see past a leading range. Returns 0 when the line starts with a
/// command-name character (the common `:w`, `:e`, `:sort` case).
pub fn range_prefix_len(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // whitespace / numbers / address atoms / separators / offsets
            b' ' | b'\t' | b'0'..=b'9' | b'%' | b'$' | b'.' | b',' | b';' | b'+' | b'-' => {
                i += 1;
            }
            // mark reference: `'` plus one (possibly multibyte) mark char
            b'\'' => {
                i += 1;
                if i < bytes.len() {
                    i += line[i..].chars().next().map_or(0, |c| c.len_utf8());
                }
            }
            // `/pat/` forward or `?pat?` backward search address
            b'/' | b'?' => {
                let delim = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != delim {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if i < bytes.len() {
                    i += 1; // closing delimiter
                }
            }
            // anything else (a command-name letter, `!`, etc.) ends the range
            _ => break,
        }
    }
    i
}

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

/// Expand a leading `~` and any `$VAR` / `${VAR}` occurrences in `s` for the
/// purpose of a directory scan. Pure/testable: `home` substitutes a leading
/// bare `~` or `~/`, and `getenv` resolves variables.
///
/// - Leading `~` (bare) or `~/…` → `home` (`~/x` → `<home>/x`). `~user` is left
///   untouched (no passwd lookup).
/// - `$NAME` / `${NAME}` anywhere → `getenv(NAME)`. Unknown variables are left
///   literally in place, so the resulting path simply won't exist and the scan
///   yields nothing rather than erroring.
///
/// This expands ONLY for the scan; callers keep the original typed prefix on the
/// returned candidates so accepting one preserves the `~` / `$VAR` the user typed.
fn expand_path_prefix(s: &str, home: &str, getenv: impl Fn(&str) -> Option<String>) -> String {
    // Expand a leading `~` (bare or `~/`) to `home`. `~user` is left as-is.
    let tilde_expanded = if s == "~" {
        home.to_string()
    } else if let Some(rest) = s.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        s.to_string()
    };

    // Expand `$NAME` / `${NAME}`; unknown vars are left literal.
    let mut out = String::with_capacity(tilde_expanded.len());
    let mut rest = tilde_expanded.as_str();
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos..]; // starts with '$'
        if let Some(braced) = after.strip_prefix("${") {
            if let Some(close) = braced.find('}') {
                let name = &braced[..close];
                match getenv(name) {
                    Some(val) => out.push_str(&val),
                    None => {
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &braced[close + 1..];
                continue;
            }
            // No closing brace → treat `$` literally, keep scanning.
            out.push('$');
            rest = &after[1..];
            continue;
        }
        // `$NAME` — name is a run of `[A-Za-z0-9_]` (all ASCII, 1 byte each).
        let name_len = after[1..]
            .bytes()
            .take_while(|b| *b == b'_' || b.is_ascii_alphanumeric())
            .count();
        if name_len > 0 {
            let name = &after[1..1 + name_len];
            match getenv(name) {
                Some(val) => out.push_str(&val),
                None => {
                    out.push('$');
                    out.push_str(name);
                }
            }
            rest = &after[1 + name_len..];
        } else {
            // Lone `$` → literal.
            out.push('$');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Scan `cwd` for entries whose names begin with `file_part` (respecting the
/// `dir_part` prefix).  Appends `/` to directories.  Hidden entries (starting
/// with `.`) are skipped unless `file_part` itself starts with `.`.
///
/// A leading `~` / `~/` and `$VAR` / `${VAR}` in the directory portion are
/// expanded for the scan only; the returned candidates keep the original typed
/// `dir_part`, so accepting one preserves the `~` / `$VAR` the user typed.
fn complete_path_entries(prefix: &str, cwd: &std::path::Path) -> Vec<String> {
    // A bare `~` scans the home dir with entries prefixed `~/`.
    let prefix = if prefix == "~" { "~/" } else { prefix };
    // Split prefix at the last '/' into (dir_part, file_part).
    let (dir_part, file_part) = match prefix.rfind('/') {
        Some(idx) => (&prefix[..=idx], &prefix[idx + 1..]),
        None => ("", prefix),
    };
    // Expand `~`/`$VAR` in the dir part for the scan; candidates keep `dir_part`.
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded_dir = expand_path_prefix(dir_part, &home, |k| std::env::var(k).ok());
    let scan_dir = if expanded_dir.is_empty() {
        cwd.to_path_buf()
    } else if std::path::Path::new(&expanded_dir).is_absolute() {
        std::path::PathBuf::from(&expanded_dir)
    } else {
        cwd.join(&expanded_dir)
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
        ArgKind::View => "<buffer>",
        ArgKind::Setting => "<setting>",
        ArgKind::Register => "<register>",
        ArgKind::Mark => "<mark>",
        ArgKind::Colorscheme => "<colorscheme>",
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
    // Skip past any leading range/count prefix so `:%sort`, `:2d`, `:'a,'bmove`
    // still resolve the command name. Offsets below are shifted back by it.
    let range_len = range_prefix_len(line);
    let sub = &line[range_len..];
    let (cmd_token_end, _) = first_word_end(sub);
    // Caret inside the range prefix, or past the command-name token → no meta.
    if caret < range_len || caret - range_len > cmd_token_end {
        return (caret..caret, vec![]);
    }
    let sub_caret = caret - range_len;
    // Gather all candidate names, same as the command-name path in complete().
    let mut names = collect_host_registry_names(host_reg);
    names.extend(collect_registry_names(editor_reg));
    names.sort();
    names.dedup();
    // Filter to those that start with the typed prefix.
    let prefix = &sub[..sub_caret];
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
    (range_len..range_len + cmd_token_end, candidates)
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
            if let Some(eq) = prefix.find('=') {
                // `name=value` — complete VALUES for the option, scoping the
                // replace range to ONLY the value token (after the `=`) so
                // accepting inserts the value, not the whole `name=`.
                let name = &prefix[..eq];
                let value_prefix = &prefix[eq + 1..];
                let value_start = token_start + eq + 1;
                let mut c: Vec<String> = crate::setting_value_candidates(name)
                    .into_iter()
                    .filter(|v| v.starts_with(value_prefix))
                    .map(|v| v.to_string())
                    .collect();
                c.sort();
                c.dedup();
                return Completions {
                    replace_range: value_start..caret,
                    candidates: c,
                    kind: CompletionKind::SettingValue,
                };
            }
            if let Some(rest) = prefix
                .strip_prefix("no")
                .or_else(|| prefix.strip_prefix("inv"))
            {
                // `no…` / `inv…` — offer the boolean option names carrying the
                // typed `no`/`inv` prefix (vim's `:set no<Tab>` behaviour).
                let toggle = &prefix[..prefix.len() - rest.len()]; // "no" or "inv"
                let mut c: Vec<String> = crate::boolean_setting_names()
                    .into_iter()
                    .map(|n| format!("{toggle}{n}"))
                    .filter(|n| n.starts_with(prefix))
                    .collect();
                c.sort();
                c.dedup();
                (c, CompletionKind::Setting)
            } else {
                // Bare option-name completion (unchanged).
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
        }
        ArgKind::View => {
            let mut c: Vec<String> = sources
                .buffers
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::View)
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
        ArgKind::Colorscheme => {
            let mut c: Vec<String> = sources
                .colorschemes
                .iter()
                .filter(|s| s.starts_with(prefix))
                .cloned()
                .collect();
            c.sort();
            c.dedup();
            (c, CompletionKind::Colorscheme)
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
    let caret = caret.min(line.len());
    // Skip a leading range/count prefix for the command-NAME position only; the
    // arg path below is unchanged (still keyed off the full line).
    let range_len = range_prefix_len(line);
    let sub = &line[range_len..];
    let (sub_cmd_end, _) = first_word_end(sub);
    if caret >= range_len && caret - range_len <= sub_cmd_end {
        // Command-name completion path (on the sub-line past the range).
        let mut names = collect_host_registry_names(host_reg);
        names.extend(collect_registry_names(editor_reg));
        names.sort();
        names.dedup();
        let mut result = complete_command_from_names(sub, caret - range_len, &names);
        // Shift the replace range back over the stripped range prefix so the
        // accepted text replaces only the command-name token, not the range.
        result.replace_range.start += range_len;
        result.replace_range.end += range_len;
        return result;
    }
    let (cmd_token_end, has_arg_space) = first_word_end(line);
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

    #[test]
    fn lcp_partial_multibyte_prefix_does_not_panic() {
        // Regression: "à" (C3 A0) and "á" (C3 A1) share only the C3 lead byte;
        // slicing at the byte-match count split the char and panicked.
        let candidates = names(&["à.txt", "á.txt"]);
        assert_eq!(longest_common_prefix(&candidates), "");
        // Shared full multibyte prefix must survive intact.
        let candidates = names(&["日本語", "日本人"]);
        assert_eq!(longest_common_prefix(&candidates), "日本");
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

    // ── :set value + no/inv completion (issue #306) ───────────────────────────

    fn setting_names() -> Vec<String> {
        crate::all_setting_names()
    }

    #[test]
    fn complete_set_value_background_offers_dark_light() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set background=` → dark, light; replace range starts AFTER the `=`.
        let line = "set background=";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::SettingValue);
        assert_eq!(
            result.candidates,
            vec!["dark".to_string(), "light".to_string()]
        );
        // "set background=" is 15 bytes; the value token is the empty slice at 15.
        assert_eq!(result.replace_range, 15..15);
    }

    #[test]
    fn complete_set_value_filters_and_scopes_to_value_token() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set background=d` → only `dark`; replace range covers ONLY `d`, so
        // accepting yields `background=dark`, not `dark`.
        let line = "set background=d";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::SettingValue);
        assert_eq!(result.candidates, vec!["dark".to_string()]);
        // The `d` sits at byte 15; replace range is 15..16 (the value only).
        assert_eq!(result.replace_range, 15..16);
        assert_eq!(&line[result.replace_range.clone()], "d");
    }

    #[test]
    fn complete_set_value_foldmethod() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set foldmethod=` → the real accepted values.
        let line = "set foldmethod=";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::SettingValue);
        assert_eq!(
            result.candidates,
            vec![
                "expr".to_string(),
                "manual".to_string(),
                "marker".to_string(),
                "syntax".to_string(),
            ]
        );
        // `:set foldmethod=mar` → only `marker` (hjkl has no `indent` value).
        let line = "set foldmethod=mar";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.candidates, vec!["marker".to_string()]);
    }

    #[test]
    fn complete_set_value_alias_and_signcolumn() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // Alias resolves too: `:set scl=` → auto/no/yes.
        let line = "set scl=";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(
            result.candidates,
            vec!["auto".to_string(), "no".to_string(), "yes".to_string()]
        );
    }

    #[test]
    fn complete_set_value_non_enum_is_empty() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // A numeric option has no value candidates.
        let line = "set tabstop=";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::SettingValue);
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn complete_set_no_prefix_offers_boolean_negations() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set no` → boolean names carrying the `no` prefix.
        let line = "set no";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::Setting);
        assert!(result.candidates.contains(&"nonumber".to_string()));
        assert!(result.candidates.contains(&"noignorecase".to_string()));
        assert!(result.candidates.contains(&"nofoldenable".to_string()));
        // Every candidate must carry the `no` prefix.
        assert!(result.candidates.iter().all(|c| c.starts_with("no")));
        // A numeric option name is not offered as a `no…` toggle.
        assert!(!result.candidates.contains(&"notabstop".to_string()));
    }

    #[test]
    fn complete_set_inv_prefix_offers_boolean_inversions() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set inv` → boolean names carrying the `inv` prefix.
        let line = "set inv";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::Setting);
        assert!(result.candidates.contains(&"invnumber".to_string()));
        assert!(result.candidates.iter().all(|c| c.starts_with("inv")));
        // Filtered further: `:set invnu` → invnumber only.
        let line = "set invnu";
        let result = complete_arg(line, line.len(), ArgKind::Setting, &sources);
        assert_eq!(result.candidates, vec!["invnumber".to_string()]);
    }

    #[test]
    fn complete_set_name_completion_unchanged_regression() {
        let settings = setting_names();
        let sources = ArgSources {
            settings: &settings,
            ..Default::default()
        };
        // `:set ` → all names (bare name case, unchanged).
        let result = complete_arg("set ", 4, ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::Setting);
        assert!(result.candidates.contains(&"foldmethod".to_string()));
        assert!(result.candidates.contains(&"number".to_string()));
        assert_eq!(result.replace_range, 4..4);

        // `:set fold` → the fold-family names (bare name case, unchanged).
        let result = complete_arg("set fold", 8, ArgKind::Setting, &sources);
        assert_eq!(result.kind, CompletionKind::Setting);
        assert!(result.candidates.contains(&"foldmethod".to_string()));
        assert!(result.candidates.contains(&"foldenable".to_string()));
        assert!(result.candidates.iter().all(|c| c.starts_with("fold")));
        assert_eq!(result.replace_range, 4..8);
    }

    #[test]
    fn complete_buffer_filters_buffers() {
        let buffers = str_vec(&["src/main.rs", "src/lib.rs", "tests/foo.rs"]);
        let sources = ArgSources {
            buffers: &buffers,
            ..Default::default()
        };
        let result = complete_arg("b ", 2, ArgKind::View, &sources);
        assert_eq!(result.kind, CompletionKind::View);
        assert!(result.candidates.contains(&"src/main.rs".to_string()));
        assert!(result.candidates.contains(&"src/lib.rs".to_string()));
        assert!(result.candidates.contains(&"tests/foo.rs".to_string()));

        let result2 = complete_arg("b src", 5, ArgKind::View, &sources);
        assert_eq!(result2.kind, CompletionKind::View);
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
            _: &mut hjkl_engine::Editor<hjkl_buffer::View, DefaultHost>,
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
        assert_eq!(arg_kind_usage(ArgKind::View), "<buffer>");
        assert_eq!(arg_kind_usage(ArgKind::Setting), "<setting>");
        assert_eq!(arg_kind_usage(ArgKind::Register), "<register>");
        assert_eq!(arg_kind_usage(ArgKind::Mark), "<mark>");
        assert_eq!(arg_kind_usage(ArgKind::Colorscheme), "<colorscheme>");
        assert_eq!(arg_kind_usage(ArgKind::Raw), "<args>");
    }

    #[test]
    fn complete_colorscheme_filters() {
        let schemes = str_vec(&["dark", "light", "tokyonight", "gruvbox", "nord"]);
        let sources = ArgSources {
            colorschemes: &schemes,
            ..Default::default()
        };
        // Empty prefix → all bundled names.
        let result = complete_arg("colorscheme ", 12, ArgKind::Colorscheme, &sources);
        assert_eq!(result.kind, CompletionKind::Colorscheme);
        assert!(result.candidates.contains(&"dark".to_string()));
        assert!(result.candidates.contains(&"tokyonight".to_string()));
        assert_eq!(result.candidates.len(), 5);

        // Prefix "tok" → only tokyonight.
        let result2 = complete_arg("colorscheme tok", 15, ArgKind::Colorscheme, &sources);
        assert_eq!(result2.candidates, vec!["tokyonight".to_string()]);
    }

    #[test]
    fn complete_command_meta_returns_arg_kinds() {
        use crate::{ExCommand, Registry};
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::View, DefaultHost>,
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
            _: &mut hjkl_engine::Editor<hjkl_buffer::View, DefaultHost>,
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

    // ── ~ / $VAR path-prefix expansion (issue #305) ───────────────────────────

    #[test]
    fn expand_path_prefix_tilde() {
        let home = "/home/me";
        let none = |_: &str| None;
        // Bare `~` and `~/` expand to home; `~/sub/` keeps the tail.
        assert_eq!(expand_path_prefix("~", home, none), "/home/me");
        assert_eq!(expand_path_prefix("~/", home, none), "/home/me/");
        assert_eq!(expand_path_prefix("~/sub/", home, none), "/home/me/sub/");
        // `~user` is left untouched (no passwd lookup).
        assert_eq!(expand_path_prefix("~bob/x/", home, none), "~bob/x/");
        // A `~` not at the start is a literal char, not a home marker.
        assert_eq!(expand_path_prefix("a/~/", home, none), "a/~/");
    }

    #[test]
    fn expand_path_prefix_vars() {
        let get = |k: &str| (k == "FOO").then(|| "/opt/foo".to_string());
        // `$VAR` and `${VAR}` both expand.
        assert_eq!(expand_path_prefix("$FOO/", "", get), "/opt/foo/");
        assert_eq!(expand_path_prefix("${FOO}/bar/", "", get), "/opt/foo/bar/");
        // A var mid-prefix expands in place.
        assert_eq!(expand_path_prefix("pre/$FOO/", "", get), "pre//opt/foo/");
        // Unknown vars are left literal (so the scan simply finds nothing).
        assert_eq!(expand_path_prefix("$NOPE/", "", get), "$NOPE/");
        assert_eq!(expand_path_prefix("${NOPE}/", "", get), "${NOPE}/");
        // Lone `$` and empty braces stay literal.
        assert_eq!(expand_path_prefix("$", "", get), "$");
        assert_eq!(expand_path_prefix("$/x", "", get), "$/x");
    }

    #[test]
    fn expand_path_prefix_tilde_then_var() {
        let home = "/home/me";
        let get = |k: &str| (k == "SUB").then(|| "docs".to_string());
        // Leading `~` expands first, then the `$VAR` in the tail.
        assert_eq!(expand_path_prefix("~/$SUB/", home, get), "/home/me/docs/");
    }

    #[test]
    fn complete_path_entries_expands_tilde_and_preserves_prefix() {
        // Point HOME at a temp dir with a known layout, then assert the returned
        // candidates keep the typed `~/` prefix (not the expanded absolute path).
        // Env is process-global; only this test touches HOME, and every other
        // path test uses a non-`~` prefix (HOME is read but unused for those),
        // so a transient HOME here can't perturb them.
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Documents")).unwrap();
        std::fs::write(home.path().join("notes.txt"), b"x").unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let prev = std::env::var("HOME").ok();
        // SAFETY: single-threaded within this test's logic; see comment above.
        unsafe { std::env::set_var("HOME", home.path()) };

        let all = complete_path_entries("~/", cwd.path());
        assert!(
            all.contains(&"~/Documents/".to_string()),
            "expected ~/Documents/ in {all:?}"
        );
        assert!(
            all.contains(&"~/notes.txt".to_string()),
            "expected ~/notes.txt in {all:?}"
        );

        // Filtering on a partial tail keeps the `~/` prefix too.
        let docs = complete_path_entries("~/Doc", cwd.path());
        assert_eq!(docs, vec!["~/Documents/".to_string()]);

        // A `$HOME/`-style prefix expands and preserves the typed `$HOME/`.
        let via_var = complete_path_entries("$HOME/no", cwd.path());
        assert_eq!(via_var, vec!["$HOME/notes.txt".to_string()]);

        // Unknown var → directory doesn't exist → no candidates.
        let nope = complete_path_entries("$NOPE_305/", cwd.path());
        assert!(
            nope.is_empty(),
            "expected empty for unknown var, got {nope:?}"
        );

        // SAFETY: restore prior HOME.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    // ── leading range/count before command name (issue #305) ──────────────────

    #[test]
    fn range_prefix_len_cases() {
        assert_eq!(range_prefix_len("sort"), 0);
        assert_eq!(range_prefix_len("w file.txt"), 0);
        assert_eq!(range_prefix_len("%sort"), 1);
        assert_eq!(range_prefix_len("2delete"), 1);
        assert_eq!(range_prefix_len("2,5d"), 3);
        assert_eq!(range_prefix_len(".,$d"), 3);
        assert_eq!(range_prefix_len("'a,'bmove"), 5);
        assert_eq!(range_prefix_len("/pat/d"), 5);
        assert_eq!(range_prefix_len("+3t"), 2);
    }

    fn range_test_registry() -> Registry<hjkl_engine::DefaultHost> {
        use crate::ExCommand;
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::View, DefaultHost>,
            _: &str,
            _: Option<crate::range::LineRange>,
        ) -> Option<crate::effect::ExEffect> {
            None
        }

        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "sort",
            aliases: &[],
            arg_kind: ArgKind::None,
            min_prefix: 3,
            run: noop,
        });
        reg.add(ExCommand {
            name: "delete",
            aliases: &["d"],
            arg_kind: ArgKind::None,
            min_prefix: 1,
            run: noop,
        });
        reg.add(ExCommand {
            name: "move",
            aliases: &["m"],
            arg_kind: ArgKind::None,
            min_prefix: 1,
            run: noop,
        });
        reg
    }

    #[test]
    fn complete_strips_leading_range_for_command_name() {
        let reg = range_test_registry();
        let host_reg = HostRegistry::<()>::new();
        let sources = ArgSources::default();

        // `%sor` → completes `sort`, replace range starts AFTER the `%`.
        let r = complete("%sor", 4, &reg, &host_reg, &sources);
        assert_eq!(r.kind, CompletionKind::Command);
        assert_eq!(r.replace_range, 1..4);
        assert!(r.candidates.contains(&"sort".to_string()));

        // `2d` → completes delete/d, replace range starts AFTER the `2`.
        let r = complete("2d", 2, &reg, &host_reg, &sources);
        assert_eq!(r.kind, CompletionKind::Command);
        assert_eq!(r.replace_range, 1..2);
        assert!(r.candidates.contains(&"delete".to_string()));
        assert!(r.candidates.contains(&"d".to_string()));

        // `'a,'bmov` → completes `move`, replace range starts after the range.
        let r = complete("'a,'bmov", 8, &reg, &host_reg, &sources);
        assert_eq!(r.kind, CompletionKind::Command);
        assert_eq!(r.replace_range, 5..8);
        assert!(r.candidates.contains(&"move".to_string()));
    }

    #[test]
    fn complete_command_meta_strips_leading_range() {
        let reg = range_test_registry();
        let host_reg = HostRegistry::<()>::new();

        let (range, cands) = complete_command_meta("%sor", 4, &reg, &host_reg);
        assert_eq!(range, 1..4);
        assert!(cands.iter().any(|c| c.name == "sort"));
    }

    // ── :put → Register completion (issue #305) ───────────────────────────────

    #[test]
    fn complete_put_completes_registers() {
        use crate::ExCommand;
        use hjkl_engine::DefaultHost;

        fn noop(
            _: &mut hjkl_engine::Editor<hjkl_buffer::View, DefaultHost>,
            _: &str,
            _: Option<crate::range::LineRange>,
        ) -> Option<crate::effect::ExEffect> {
            None
        }

        let mut reg = Registry::<DefaultHost>::new();
        reg.add(ExCommand {
            name: "put",
            aliases: &["pu"],
            arg_kind: ArgKind::Register,
            min_prefix: 2,
            run: noop,
        });
        let host_reg = HostRegistry::<()>::new();
        let regs = str_vec(&["\"\"", "\"0", "\"a", "\"b"]);
        let sources = ArgSources {
            registers: &regs,
            ..Default::default()
        };

        // `:put ` → all register selectors.
        let r = complete("put ", 4, &reg, &host_reg, &sources);
        assert_eq!(r.kind, CompletionKind::Register);
        assert!(r.candidates.contains(&"\"a".to_string()));
        assert!(r.candidates.contains(&"\"b".to_string()));

        // `:put "a` → filters to `"a`.
        let r2 = complete("put \"a", 6, &reg, &host_reg, &sources);
        assert_eq!(r2.kind, CompletionKind::Register);
        assert!(r2.candidates.contains(&"\"a".to_string()));
        assert!(!r2.candidates.contains(&"\"b".to_string()));
    }
}
