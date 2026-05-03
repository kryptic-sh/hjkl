use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "swift",
    language: tree_sitter_swift::LANGUAGE.into(),
    highlights_scm: tree_sitter_swift::HIGHLIGHTS_QUERY,
    extensions: &["swift"],
});
