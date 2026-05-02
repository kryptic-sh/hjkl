use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for TSX.
/// highlights.scm sourced from `tree_sitter_typescript::HIGHLIGHTS_QUERY` (upstream constant).
/// Uses `LANGUAGE_TSX` — the crate bundles both TS and TSX grammars under one dep.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "tsx",
    language: tree_sitter_typescript::LANGUAGE_TSX.into(),
    highlights_scm: tree_sitter_typescript::HIGHLIGHTS_QUERY,
    extensions: &["tsx"],
});
