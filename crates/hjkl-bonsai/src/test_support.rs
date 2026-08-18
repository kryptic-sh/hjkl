//! Test-only fixture builders shared across the crate's test modules, so the
//! pinned grammar revisions (and the C grammar spec) live in exactly one place
//! instead of being duplicated verbatim in four files.
#![cfg(test)]

use crate::runtime::{LangSpec, ManifestMeta, QuerySource};

/// The pinned manifest revisions every network-touching test installs against
/// (the helix and nvim-treesitter query repos).
pub fn pinned_manifest_meta() -> ManifestMeta {
    ManifestMeta {
        helix_repo: "https://github.com/helix-editor/helix".into(),
        helix_rev: "87d5c05c4432a079d3b7aaa10cda1cfe1803c18c".into(),
        nvim_treesitter_repo: "https://github.com/nvim-treesitter/nvim-treesitter".into(),
        nvim_treesitter_rev: "cf12346a3414fa1b06af75c79faebe7f76df080a".into(),
    }
}

/// The pinned C grammar spec used by the grammar-loading tests.
pub fn c_lang_spec() -> LangSpec {
    LangSpec {
        git_url: "https://github.com/tree-sitter/tree-sitter-c".into(),
        git_rev: "2a265d69a4caf57108a73ad2ed1e6922dd2f998c".into(),
        subpath: None,
        extensions: vec!["c".into()],
        c_files: vec!["src/parser.c".into()],
        query_source: QuerySource::Helix,
        query_subdir: None,
        source: None,
    }
}
