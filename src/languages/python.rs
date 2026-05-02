use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for Python.
/// highlights.scm sourced from `tree_sitter_python::HIGHLIGHTS_QUERY` (upstream constant).
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "python",
    language: tree_sitter_python::LANGUAGE.into(),
    highlights_scm: tree_sitter_python::HIGHLIGHTS_QUERY,
    extensions: &["py", "pyi"],
});
