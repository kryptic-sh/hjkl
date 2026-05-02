use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for CSS.
/// highlights.scm sourced from `tree_sitter_css::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "css",
    language: tree_sitter_css::LANGUAGE.into(),
    highlights_scm: tree_sitter_css::HIGHLIGHTS_QUERY,
    extensions: &["css"],
});
