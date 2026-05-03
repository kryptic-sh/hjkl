use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for JavaScript.
/// highlights.scm sourced from `tree_sitter_javascript::HIGHLIGHTS_QUERY` (upstream constant).
/// Covers `.js`, `.mjs`, `.cjs`, and `.jsx` files (the same grammar handles JSX).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "javascript",
    language: tree_sitter_javascript::LANGUAGE.into(),
    highlights_scm: tree_sitter_javascript::HIGHLIGHT_QUERY,
    extensions: &["js", "mjs", "cjs", "jsx"],
});
