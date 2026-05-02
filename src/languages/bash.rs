use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for Bash.
/// highlights.scm sourced from `tree_sitter_bash::HIGHLIGHT_QUERY` (upstream constant — no trailing S).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "bash",
    language: tree_sitter_bash::LANGUAGE.into(),
    highlights_scm: tree_sitter_bash::HIGHLIGHT_QUERY,
    extensions: &["sh", "bash"],
});
