use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// R — extension match is case-sensitive; both `.r` and `.R` map here.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "r",
    language: tree_sitter_r::LANGUAGE.into(),
    highlights_scm: tree_sitter_r::HIGHLIGHTS_QUERY,
    extensions: &["r", "R"],
});
