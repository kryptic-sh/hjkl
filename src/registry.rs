use std::path::Path;

/// Static configuration for a single language.
pub struct LanguageConfig {
    /// Short name, e.g. `"rust"`.
    pub name: &'static str,
    /// The tree-sitter `Language` for this grammar.
    pub language: tree_sitter::Language,
    /// The highlights.scm query source (from upstream crate constant or vendored).
    pub highlights_scm: &'static str,
    /// File extensions that map to this language, e.g. `&["rs"]`.
    pub extensions: &'static [&'static str],
}

/// Registry mapping file extensions and language names to `LanguageConfig`.
pub struct LanguageRegistry {
    configs: Vec<&'static LanguageConfig>,
}

impl LanguageRegistry {
    /// Build the default registry with all bundled languages.
    pub fn new() -> Self {
        use crate::languages;
        Self {
            configs: vec![
                &languages::rust::CONFIG,
                &languages::markdown::CONFIG,
                &languages::json::CONFIG,
                &languages::toml::CONFIG,
                &languages::sql::CONFIG,
                &languages::python::CONFIG,
                &languages::javascript::CONFIG,
                &languages::typescript::CONFIG,
                &languages::tsx::CONFIG,
                &languages::go::CONFIG,
                &languages::yaml::CONFIG,
                &languages::bash::CONFIG,
                &languages::c::CONFIG,
                &languages::html::CONFIG,
                &languages::css::CONFIG,
            ],
        }
    }

    /// Detect a language by file extension (case-sensitive, no leading dot).
    pub fn detect_for_path(&self, path: &Path) -> Option<&'static LanguageConfig> {
        let ext = path.extension()?.to_str()?;
        self.configs
            .iter()
            .find(|c| c.extensions.contains(&ext))
            .copied()
    }

    /// Look up a language by name (e.g. `"rust"`, `"json"`).
    pub fn by_name(&self, name: &str) -> Option<&'static LanguageConfig> {
        self.configs.iter().find(|c| c.name == name).copied()
    }

    /// Iterate over all registered language configs.
    pub fn all(&self) -> impl Iterator<Item = &'static LanguageConfig> + '_ {
        self.configs.iter().copied()
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience free function — detects a language config for the given path.
pub fn detect_language_for_path(path: &Path) -> Option<&'static LanguageConfig> {
    LanguageRegistry::new().detect_for_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_detects_rust_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("foo.rs")).unwrap();
        assert_eq!(cfg.name, "rust");
    }

    #[test]
    fn registry_by_name_rust() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("rust").is_some());
    }

    #[test]
    fn registry_detects_json_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("package.json")).unwrap();
        assert_eq!(cfg.name, "json");
    }

    #[test]
    fn registry_detects_toml_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("Cargo.toml")).unwrap();
        assert_eq!(cfg.name, "toml");
    }

    #[test]
    fn registry_detects_markdown_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("README.md")).unwrap();
        assert_eq!(cfg.name, "markdown");
    }

    #[test]
    fn registry_detects_sql_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("schema.sql")).unwrap();
        assert_eq!(cfg.name, "sql");
    }

    #[test]
    fn registry_unknown_extension_returns_none() {
        let reg = LanguageRegistry::new();
        assert!(reg.detect_for_path(Path::new("foo.xyz123")).is_none());
    }

    #[test]
    fn detect_language_for_path_free_fn() {
        let cfg = detect_language_for_path(Path::new("main.rs")).unwrap();
        assert_eq!(cfg.name, "rust");
    }

    #[test]
    fn registry_detects_python_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("main.py")).unwrap();
        assert_eq!(cfg.name, "python");
    }

    #[test]
    fn registry_by_name_python() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("python").is_some());
    }

    #[test]
    fn registry_detects_javascript_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("app.js")).unwrap();
        assert_eq!(cfg.name, "javascript");
        let cfg = reg.detect_for_path(Path::new("App.jsx")).unwrap();
        assert_eq!(cfg.name, "javascript");
    }

    #[test]
    fn registry_by_name_javascript() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("javascript").is_some());
    }

    #[test]
    fn registry_detects_typescript_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("app.ts")).unwrap();
        assert_eq!(cfg.name, "typescript");
    }

    #[test]
    fn registry_by_name_typescript() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("typescript").is_some());
    }

    #[test]
    fn registry_detects_tsx_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("App.tsx")).unwrap();
        assert_eq!(cfg.name, "tsx");
    }

    #[test]
    fn registry_by_name_tsx() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("tsx").is_some());
    }

    #[test]
    fn registry_detects_go_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("main.go")).unwrap();
        assert_eq!(cfg.name, "go");
    }

    #[test]
    fn registry_by_name_go() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("go").is_some());
    }

    #[test]
    fn registry_detects_yaml_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("config.yml")).unwrap();
        assert_eq!(cfg.name, "yaml");
    }

    #[test]
    fn registry_by_name_yaml() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("yaml").is_some());
    }

    #[test]
    fn registry_detects_bash_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("deploy.sh")).unwrap();
        assert_eq!(cfg.name, "bash");
    }

    #[test]
    fn registry_by_name_bash() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("bash").is_some());
    }

    #[test]
    fn registry_detects_c_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("main.c")).unwrap();
        assert_eq!(cfg.name, "c");
    }

    #[test]
    fn registry_by_name_c() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("c").is_some());
    }

    #[test]
    fn registry_detects_html_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("index.html")).unwrap();
        assert_eq!(cfg.name, "html");
    }

    #[test]
    fn registry_by_name_html() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("html").is_some());
    }

    #[test]
    fn registry_detects_css_extension() {
        let reg = LanguageRegistry::new();
        let cfg = reg.detect_for_path(Path::new("style.css")).unwrap();
        assert_eq!(cfg.name, "css");
    }

    #[test]
    fn registry_by_name_css() {
        let reg = LanguageRegistry::new();
        assert!(reg.by_name("css").is_some());
    }
}
