use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for Markdown (block grammar).
/// highlights.scm sourced from `tree_sitter_md::HIGHLIGHT_QUERY_BLOCK` (upstream constant).
/// The inline grammar is deliberately omitted — injections are out of scope for Phase B.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "markdown",
    language: tree_sitter_md::LANGUAGE.into(),
    highlights_scm: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
    extensions: &["md", "markdown"],
});
