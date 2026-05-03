use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// PHP — uses `LANGUAGE_PHP` (handles embedded HTML), not `LANGUAGE_PHP_ONLY`.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "php",
    language: tree_sitter_php::LANGUAGE_PHP.into(),
    highlights_scm: tree_sitter_php::HIGHLIGHTS_QUERY,
    extensions: &["php", "phtml"],
});
