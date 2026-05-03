use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// Language config for C++.
/// `.h` stays routed to C; only `.hpp/.hxx/.hh` route here to avoid the
/// ambiguity since most C projects also use `.h` for headers.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "cpp",
    language: tree_sitter_cpp::LANGUAGE.into(),
    highlights_scm: tree_sitter_cpp::HIGHLIGHT_QUERY,
    extensions: &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
});
