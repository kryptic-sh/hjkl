use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for Go.
/// highlights.scm sourced from `tree_sitter_go::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "go",
    language: tree_sitter_go::LANGUAGE.into(),
    highlights_scm: tree_sitter_go::HIGHLIGHTS_QUERY,
    extensions: &["go"],
});
