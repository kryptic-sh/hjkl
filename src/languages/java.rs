use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "java",
    language: tree_sitter_java::LANGUAGE.into(),
    highlights_scm: tree_sitter_java::HIGHLIGHTS_QUERY,
    extensions: &["java"],
});
