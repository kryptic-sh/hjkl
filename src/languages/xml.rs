use std::sync::LazyLock;

use crate::registry::LanguageConfig;

/// XML — `tree-sitter-xml` ships both XML and DTD; we expose XML only.
pub static CONFIG: LazyLock<LanguageConfig> = LazyLock::new(|| LanguageConfig {
    name: "xml",
    language: tree_sitter_xml::LANGUAGE_XML.into(),
    highlights_scm: tree_sitter_xml::XML_HIGHLIGHT_QUERY,
    extensions: &["xml", "xsd", "xsl", "xslt", "svg"],
});
