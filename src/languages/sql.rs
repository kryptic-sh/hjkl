use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for SQL (via tree-sitter-sequel).
/// highlights.scm sourced from `tree_sitter_sequel::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "sql",
    language: tree_sitter_sequel::LANGUAGE.into(),
    highlights_scm: tree_sitter_sequel::HIGHLIGHTS_QUERY,
    extensions: &["sql"],
});
