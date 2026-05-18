//! Parsed shape of `anvil.toml`. Mirrors hjkl-bonsai's `Manifest`/`LangSpec`
//! pattern.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

// ── Error ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("invalid tool name {0:?}: must be lowercase ASCII alphanumeric, '-', or '_'")]
    InvalidName(String),

    #[error("tool {0:?} has no sha256 checksums (github method requires at least one triple)")]
    MissingChecksums(String),

    #[error(
        "tool {tool:?} triple {triple:?}: invalid sha256 value {value:?}; \
         use exactly 64 lowercase hex chars for a pinned hash, or \"\" to opt into TOFU"
    )]
    InvalidSha256 {
        tool: String,
        triple: String,
        value: String,
    },

    #[error("tool {0:?} has an empty version string")]
    EmptyVersion(String),

    #[error("tool {0:?} has an empty bin string")]
    EmptyBin(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ── Manifest ─────────────────────────────────────────────────────────────────

/// Top-level shape of `anvil.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub meta: ManifestMeta,
    /// Tool name -> spec. Names must be lowercase ASCII identifiers
    /// (validated by [`Manifest::validate`]).
    #[serde(default)]
    pub tool: BTreeMap<String, ToolSpec>,
}

/// Metadata block at the top of `anvil.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestMeta {
    /// Version of the manifest schema. Bump when adding required fields.
    pub schema_version: u32,
    /// Pinned upstream `mason-org/mason-registry` rev that the catalog
    /// was synced from. Empty string for a hand-curated test catalog.
    #[serde(default)]
    pub upstream_rev: String,
}

/// One tool entry.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolSpec {
    pub category: ToolCategory,
    pub description: String,
    /// Pinned version string. Format depends on the install method.
    pub version: String,
    /// Final binary name once installed (e.g. `rust-analyzer`, `pyright`).
    pub bin: String,
    /// One of the install methods.
    #[serde(flatten)]
    pub method: InstallMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCategory {
    Lsp,
    Formatter,
    Linter,
    Dap,
}

/// Tagged on the `method` field — `method = "github"` etc.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum InstallMethod {
    Github(GithubMethod),
    Cargo(CargoMethod),
    Npm(NpmMethod),
    Pip(PipMethod),
    GoInstall(GoMethod),
    Script(ScriptMethod),
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubMethod {
    /// `owner/repo`.
    pub repo: String,
    /// Asset filename pattern with `{triple}` and `{version}` substitutions.
    /// Example: `rust-analyzer-{triple}.gz`.
    pub asset_pattern: String,
    /// Per-triple SHA-256 checksums. Keys: `x86_64-unknown-linux-gnu`,
    /// `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
    /// `x86_64-apple-darwin`, `aarch64-apple-darwin`,
    /// `x86_64-pc-windows-msvc`.
    pub sha256: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CargoMethod {
    pub crate_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NpmMethod {
    pub package: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipMethod {
    pub package: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoMethod {
    pub module: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScriptMethod {
    pub url: String,
    pub sha256: String,
    /// Shell command run after extraction, relative to the staging dir.
    pub exec: String,
}

// ── Validation ───────────────────────────────────────────────────────────────

impl Manifest {
    /// Validate all tool entries in the manifest.
    ///
    /// - Tool names must be lowercase ASCII alphanumeric + `-` + `_`.
    /// - `bin` and `version` must be non-empty per entry.
    /// - `Github` entries must supply at least one SHA-256 triple.
    pub fn validate(&self) -> Result<(), ManifestError> {
        for (name, spec) in &self.tool {
            // Name validation: lowercase ASCII alnum + '-' + '_'
            if !is_valid_tool_name(name) {
                return Err(ManifestError::InvalidName(name.clone()));
            }
            // Non-empty version
            if spec.version.is_empty() {
                return Err(ManifestError::EmptyVersion(name.clone()));
            }
            // Non-empty bin
            if spec.bin.is_empty() {
                return Err(ManifestError::EmptyBin(name.clone()));
            }
            // Github: validate each sha256 value.
            // - "" → TOFU (allowed)
            // - exactly 64 lowercase hex chars → pinned (allowed)
            // - anything else (including all-zero placeholders) → rejected
            if let InstallMethod::Github(ref g) = spec.method {
                if g.sha256.is_empty() {
                    return Err(ManifestError::MissingChecksums(name.clone()));
                }
                for (triple, value) in &g.sha256 {
                    if !value.is_empty() && !is_valid_pinned_sha256(value) {
                        return Err(ManifestError::InvalidSha256 {
                            tool: name.clone(),
                            triple: triple.clone(),
                            value: value.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

fn is_valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Returns `true` iff `s` is exactly 64 lowercase hex chars AND is not the
/// all-zero placeholder (`"000...0"`).
///
/// - Valid pinned hash: `"deadbeef..."`  (64 hex, mixed digits/[a-f])
/// - TOFU opt-in: `""`  (empty) — handled at call site, not here
/// - Rejected: `"0000...0"` (64 zeros), uppercase hex, junk, wrong length
fn is_valid_pinned_sha256(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
        && !s.chars().all(|c| c == '0')
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse an `anvil.toml` from a string slice.
pub fn parse_str(s: &str) -> Result<Manifest, ManifestError> {
    let manifest: Manifest = toml::from_str(s)?;
    Ok(manifest)
}

/// Load and parse an `anvil.toml` from a file path.
pub fn load(path: &Path) -> Result<Manifest, ManifestError> {
    let s = std::fs::read_to_string(path)?;
    parse_str(&s)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the embedded in-tree `anvil.toml` and do a basic smoke check.
    #[test]
    fn parse_embedded_anvil_toml() {
        let s = include_str!("../anvil.toml");
        let m = parse_str(s).expect("embedded anvil.toml must parse cleanly");
        assert_eq!(m.tool.len(), 6, "expected 6 tools in embedded catalog");

        // Spot-check categories
        assert_eq!(m.tool["rust-analyzer"].category, ToolCategory::Lsp);
        assert_eq!(m.tool["shfmt"].category, ToolCategory::Formatter);

        // Validate passes for the hand-curated file
        m.validate()
            .expect("embedded anvil.toml must pass validation");
    }

    /// Each install method round-trips through `parse_str`.
    #[test]
    fn method_github_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.rust-analyzer]
            category = "lsp"
            description = "Rust language server"
            version = "2025-01-13"
            bin = "rust-analyzer"
            method = "github"
            repo = "rust-lang/rust-analyzer"
            asset_pattern = "rust-analyzer-{triple}.gz"
            [tool.rust-analyzer.sha256]
            "x86_64-unknown-linux-gnu" = "deadbeef"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("rust-analyzer").unwrap();
        assert!(
            matches!(&spec.method, InstallMethod::Github(g) if g.repo == "rust-lang/rust-analyzer")
        );
    }

    #[test]
    fn method_cargo_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.taplo]
            category = "lsp"
            description = "TOML language server"
            version = "0.9.3"
            bin = "taplo"
            method = "cargo"
            crate_name = "taplo-cli"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("taplo").unwrap();
        assert!(matches!(&spec.method, InstallMethod::Cargo(c) if c.crate_name == "taplo-cli"));
    }

    #[test]
    fn method_npm_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.pyright]
            category = "lsp"
            description = "Python type checker"
            version = "1.1.395"
            bin = "pyright-langserver"
            method = "npm"
            package = "pyright"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("pyright").unwrap();
        assert!(matches!(&spec.method, InstallMethod::Npm(n) if n.package == "pyright"));
    }

    #[test]
    fn method_pip_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.black]
            category = "formatter"
            description = "Python formatter"
            version = "24.0.0"
            bin = "black"
            method = "pip"
            package = "black"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("black").unwrap();
        assert!(matches!(&spec.method, InstallMethod::Pip(p) if p.package == "black"));
    }

    #[test]
    fn method_goinstall_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.gopls]
            category = "lsp"
            description = "Go language server"
            version = "v0.17.1"
            bin = "gopls"
            method = "goinstall"
            module = "golang.org/x/tools/gopls"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("gopls").unwrap();
        assert!(
            matches!(&spec.method, InstallMethod::GoInstall(g) if g.module == "golang.org/x/tools/gopls")
        );
    }

    #[test]
    fn method_script_roundtrip() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.somescript]
            category = "lsp"
            description = "Script-installed tool"
            version = "1.0.0"
            bin = "somescript"
            method = "script"
            url = "https://example.com/install.tar.gz"
            sha256 = "deadbeef00000000000000000000000000000000000000000000000000000000"
            exec = "./install.sh"
        "#;
        let m = parse_str(toml).unwrap();
        let spec = m.tool.get("somescript").unwrap();
        assert!(
            matches!(&spec.method, InstallMethod::Script(s) if s.url == "https://example.com/install.tar.gz")
        );
    }

    // ── Error variant tests ───────────────────────────────────────────────────

    #[test]
    fn error_invalid_name_uppercase() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.RustAnalyzer]
            category = "lsp"
            description = "Rust language server"
            version = "2025-01-13"
            bin = "rust-analyzer"
            method = "cargo"
            crate_name = "rust-analyzer"
        "#;
        let m = parse_str(toml).unwrap();
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::InvalidName(_)));
    }

    #[test]
    fn error_invalid_name_space() {
        let toml = r#"
            [meta]
            schema_version = 1
        "#;
        // We can't put spaces in TOML keys easily, so we construct and validate manually.
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "has space".to_string(),
            ToolSpec {
                category: ToolCategory::Lsp,
                description: "test".to_string(),
                version: "1.0".to_string(),
                bin: "bin".to_string(),
                method: InstallMethod::Cargo(CargoMethod {
                    crate_name: "test".to_string(),
                }),
            },
        );
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::InvalidName(_)));
    }

    #[test]
    fn error_empty_version() {
        let toml = r#"
            [meta]
            schema_version = 1
        "#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            ToolSpec {
                category: ToolCategory::Lsp,
                description: "test".to_string(),
                version: String::new(),
                bin: "my-tool".to_string(),
                method: InstallMethod::Cargo(CargoMethod {
                    crate_name: "my-tool".to_string(),
                }),
            },
        );
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::EmptyVersion(_)));
    }

    #[test]
    fn error_empty_bin() {
        let toml = r#"
            [meta]
            schema_version = 1
        "#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            ToolSpec {
                category: ToolCategory::Lsp,
                description: "test".to_string(),
                version: "1.0".to_string(),
                bin: String::new(),
                method: InstallMethod::Cargo(CargoMethod {
                    crate_name: "my-tool".to_string(),
                }),
            },
        );
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::EmptyBin(_)));
    }

    #[test]
    fn error_missing_checksums_github() {
        let toml = r#"
            [meta]
            schema_version = 1
        "#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            ToolSpec {
                category: ToolCategory::Lsp,
                description: "test".to_string(),
                version: "1.0".to_string(),
                bin: "my-tool".to_string(),
                method: InstallMethod::Github(GithubMethod {
                    repo: "owner/repo".to_string(),
                    asset_pattern: "tool-{triple}.tar.gz".to_string(),
                    sha256: BTreeMap::new(),
                }),
            },
        );
        let err = m.validate().unwrap_err();
        assert!(matches!(err, ManifestError::MissingChecksums(_)));
    }

    // ── SHA-256 value validation tests ────────────────────────────────────────

    fn github_tool_with_sha(triple: &str, sha_value: &str) -> ToolSpec {
        let mut sha256 = BTreeMap::new();
        sha256.insert(triple.to_string(), sha_value.to_string());
        ToolSpec {
            category: ToolCategory::Lsp,
            description: "test".to_string(),
            version: "1.0".to_string(),
            bin: "my-tool".to_string(),
            method: InstallMethod::Github(GithubMethod {
                repo: "owner/repo".to_string(),
                asset_pattern: "tool-{triple}.tar.gz".to_string(),
                sha256,
            }),
        }
    }

    #[test]
    fn sha256_empty_string_accepted_as_tofu() {
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            github_tool_with_sha("x86_64-unknown-linux-gnu", ""),
        );
        m.validate()
            .expect("empty string sha256 must be accepted as TOFU opt-in");
    }

    #[test]
    fn sha256_valid_64_hex_pinned_accepted() {
        let valid_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(valid_sha.len(), 64);
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            github_tool_with_sha("x86_64-unknown-linux-gnu", valid_sha),
        );
        m.validate()
            .expect("64-char lowercase hex sha256 must be accepted as pinned");
    }

    #[test]
    fn sha256_all_zeros_rejected() {
        let zeros = "0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(zeros.len(), 64);
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            github_tool_with_sha("x86_64-unknown-linux-gnu", zeros),
        );
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestError::InvalidSha256 { .. }),
            "all-zero sha256 must be rejected; got: {err:?}"
        );
    }

    #[test]
    fn sha256_junk_string_rejected() {
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            github_tool_with_sha("x86_64-unknown-linux-gnu", "notahexhash"),
        );
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestError::InvalidSha256 { .. }),
            "junk sha256 value must be rejected; got: {err:?}"
        );
    }

    #[test]
    fn sha256_uppercase_hex_rejected() {
        // Must be lowercase hex — uppercase is invalid per spec.
        let uppercase = "DEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF";
        assert_eq!(uppercase.len(), 64);
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            github_tool_with_sha("x86_64-unknown-linux-gnu", uppercase),
        );
        let err = m.validate().unwrap_err();
        assert!(
            matches!(err, ManifestError::InvalidSha256 { .. }),
            "uppercase sha256 must be rejected; got: {err:?}"
        );
    }

    #[test]
    fn sha256_mixed_valid_and_tofu_accepted() {
        // One triple pinned, one empty (TOFU) — both allowed in the same map.
        let valid_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let mut sha256 = BTreeMap::new();
        sha256.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            valid_sha.to_string(),
        );
        sha256.insert("aarch64-apple-darwin".to_string(), "".to_string());
        let toml = r#"[meta]
schema_version = 1
"#;
        let mut m = parse_str(toml).unwrap();
        m.tool.insert(
            "my-tool".to_string(),
            ToolSpec {
                category: ToolCategory::Lsp,
                description: "test".to_string(),
                version: "1.0".to_string(),
                bin: "my-tool".to_string(),
                method: InstallMethod::Github(GithubMethod {
                    repo: "owner/repo".to_string(),
                    asset_pattern: "tool-{triple}.tar.gz".to_string(),
                    sha256,
                }),
            },
        );
        m.validate()
            .expect("mix of pinned + TOFU sha256 values must be accepted");
    }

    #[test]
    fn error_toml_parse_error() {
        let bad_toml = "this is not valid toml ={{{";
        let err = parse_str(bad_toml).unwrap_err();
        assert!(matches!(err, ManifestError::Toml(_)));
    }

    #[test]
    fn error_io_missing_file() {
        let err = load(Path::new("/nonexistent/path/anvil.toml")).unwrap_err();
        assert!(matches!(err, ManifestError::Io(_)));
    }

    #[test]
    fn load_from_tempfile() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
            [meta]
            schema_version = 1

            [tool.taplo]
            category = "lsp"
            description = "TOML ls"
            version = "0.9.3"
            bin = "taplo"
            method = "cargo"
            crate_name = "taplo-cli"
        "#
        )
        .unwrap();
        let m = load(f.path()).unwrap();
        assert!(m.tool.contains_key("taplo"));
    }

    #[test]
    fn all_categories_parse() {
        let toml = r#"
            [meta]
            schema_version = 1

            [tool.tool-lsp]
            category = "lsp"
            description = "d"
            version = "1"
            bin = "b"
            method = "cargo"
            crate_name = "c"

            [tool.tool-formatter]
            category = "formatter"
            description = "d"
            version = "1"
            bin = "b"
            method = "cargo"
            crate_name = "c"

            [tool.tool-linter]
            category = "linter"
            description = "d"
            version = "1"
            bin = "b"
            method = "cargo"
            crate_name = "c"

            [tool.tool-dap]
            category = "dap"
            description = "d"
            version = "1"
            bin = "b"
            method = "cargo"
            crate_name = "c"
        "#;
        let m = parse_str(toml).unwrap();
        assert_eq!(m.tool["tool-lsp"].category, ToolCategory::Lsp);
        assert_eq!(m.tool["tool-formatter"].category, ToolCategory::Formatter);
        assert_eq!(m.tool["tool-linter"].category, ToolCategory::Linter);
        assert_eq!(m.tool["tool-dap"].category, ToolCategory::Dap);
    }

    #[test]
    fn valid_names_pass() {
        assert!(is_valid_tool_name("rust-analyzer"));
        assert!(is_valid_tool_name("lua_ls"));
        assert!(is_valid_tool_name("gopls"));
        assert!(is_valid_tool_name("tool123"));
    }

    #[test]
    fn invalid_names_rejected() {
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("Rust-Analyzer"));
        assert!(!is_valid_tool_name("tool name"));
        assert!(!is_valid_tool_name("tool.name"));
    }
}
