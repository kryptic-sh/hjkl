use std::sync::LazyLock;

use crate::registry::LanguageConfig;

pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "c-sharp",
    language: tree_sitter_c_sharp::LANGUAGE.into(),
    highlights_scm: tree_sitter_c_sharp::HIGHLIGHTS_QUERY,
    extensions: &["cs"],
});
