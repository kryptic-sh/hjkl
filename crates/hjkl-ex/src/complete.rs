use std::ops::Range;

use crate::{HostRegistry, Registry};

/// What kind of token is being completed. Phase 5a only emits `Command`;
/// Phase 6 adds Path/Setting/Buffer/Register/Mark for arg completion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    None,
    Command,
    // Reserved for Phase 6: Path, Setting, Buffer, Register, Mark
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
}
