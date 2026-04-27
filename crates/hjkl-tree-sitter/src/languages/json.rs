use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for JSON.
/// highlights.scm sourced from `tree_sitter_json::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "json",
    language: tree_sitter_json::LANGUAGE.into(),
    highlights_scm: tree_sitter_json::HIGHLIGHTS_QUERY,
    extensions: &["json", "jsonc"],
});
