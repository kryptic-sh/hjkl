use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for Rust.
/// highlights.scm sourced from `tree_sitter_rust::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "rust",
    language: tree_sitter_rust::LANGUAGE.into(),
    highlights_scm: tree_sitter_rust::HIGHLIGHTS_QUERY,
    extensions: &["rs"],
});
