use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for HTML.
/// highlights.scm sourced from `tree_sitter_html::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "html",
    language: tree_sitter_html::LANGUAGE.into(),
    highlights_scm: tree_sitter_html::HIGHLIGHTS_QUERY,
    extensions: &["html", "htm"],
});
