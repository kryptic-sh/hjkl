use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "dart",
    language: tree_sitter_dart::LANGUAGE.into(),
    highlights_scm: tree_sitter_dart::HIGHLIGHTS_QUERY,
    extensions: &["dart"],
});
