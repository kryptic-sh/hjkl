use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for C.
/// highlights.scm sourced from `tree_sitter_c::HIGHLIGHT_QUERY` (upstream constant — no trailing S).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "c",
    language: tree_sitter_c::LANGUAGE.into(),
    highlights_scm: tree_sitter_c::HIGHLIGHT_QUERY,
    extensions: &["c", "h"],
});
