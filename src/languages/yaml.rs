use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for YAML.
/// highlights.scm sourced from `tree_sitter_yaml::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "yaml",
    language: tree_sitter_yaml::LANGUAGE.into(),
    highlights_scm: tree_sitter_yaml::HIGHLIGHTS_QUERY,
    extensions: &["yml", "yaml"],
});
