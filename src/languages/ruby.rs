use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "ruby",
    language: tree_sitter_ruby::LANGUAGE.into(),
    highlights_scm: tree_sitter_ruby::HIGHLIGHTS_QUERY,
    extensions: &["rb", "rake", "gemspec"],
});
