use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for TypeScript.
/// highlights.scm sourced from `tree_sitter_typescript::HIGHLIGHTS_QUERY` (upstream constant).
/// Uses `LANGUAGE_TYPESCRIPT` — the crate bundles both TS and TSX grammars.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "typescript",
    language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
    highlights_scm: tree_sitter_typescript::HIGHLIGHTS_QUERY,
    extensions: &["ts", "mts", "cts"],
});
