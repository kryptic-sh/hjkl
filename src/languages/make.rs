use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Make — extension-only match for now (`*.mk`). Filename detection for
/// the canonical `Makefile` is a follow-up (same as Dockerfile).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "make",
    language: tree_sitter_make::LANGUAGE.into(),
    highlights_scm: tree_sitter_make::HIGHLIGHTS_QUERY,
    extensions: &["mk"],
});
