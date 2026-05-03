use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "lua",
    language: tree_sitter_lua::LANGUAGE.into(),
    highlights_scm: tree_sitter_lua::HIGHLIGHTS_QUERY,
    extensions: &["lua"],
});
