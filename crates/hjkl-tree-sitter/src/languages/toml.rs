use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for TOML.
/// highlights.scm sourced from `tree_sitter_toml_ng::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "toml",
    language: tree_sitter_toml_ng::LANGUAGE.into(),
    highlights_scm: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
    extensions: &["toml"],
});
