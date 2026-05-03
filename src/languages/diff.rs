use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "diff",
    language: tree_sitter_diff::LANGUAGE.into(),
    highlights_scm: tree_sitter_diff::HIGHLIGHTS_QUERY,
    extensions: &["diff", "patch"],
});
