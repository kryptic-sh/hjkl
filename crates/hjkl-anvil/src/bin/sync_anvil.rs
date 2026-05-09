// xtask binary; not part of the published crate.
//
// Populates `crates/hjkl-anvil/anvil.toml` from the upstream
// `mason-org/mason-registry` pre-compiled JSON artifact.
//
// Usage:
//   cargo run -p hjkl-anvil --features sync --bin sync-anvil -- --pin <tag>
//
// `--pin` must be a mason-registry release tag, e.g. `2025-01-01-fizzy-foo`.
// Bumping the pin is a manual step — this binary is run on demand by
// maintainers, not in CI.

use std::collections::BTreeMap;
use std::io::{Cursor, Read as _};
use std::path::PathBuf;

use clap::Parser;
use serde_json::Value;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "sync-anvil",
    about = "Populate anvil.toml from mason-org/mason-registry"
)]
struct Cli {
    /// Pinned mason-registry release tag (e.g. `2025-01-01-fizzy-foo`).
    #[arg(long)]
    pin: String,

    /// Output path for anvil.toml. Defaults to in-tree catalog.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Print summary + would-emit TOML to stdout; do not write the file.
    #[arg(long)]
    dry_run: bool,
}

// ── Triple mapping ───────────────────────────────────────────────────────────

/// Mason target → our Rust triple.
fn mason_target_to_triple(target: &str) -> Option<&'static str> {
    match target {
        "linux_x64_gnu" | "linux_x64" => Some("x86_64-unknown-linux-gnu"),
        "linux_x64_musl" => Some("x86_64-unknown-linux-musl"),
        "linux_arm64_gnu" | "linux_arm64" => Some("aarch64-unknown-linux-gnu"),
        "darwin_x64" => Some("x86_64-apple-darwin"),
        "darwin_arm64" => Some("aarch64-apple-darwin"),
        "win_x64" => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

// ── Asset-pattern inference ──────────────────────────────────────────────────

/// Try to infer a single `asset_pattern` from per-target file entries.
///
/// Each entry is `(mason_target, filename)`. Returns the inferred pattern
/// and the set of triples that were matched, or a skip reason on failure.
pub fn infer_asset_pattern(
    entries: &[(String, String)],
) -> Result<(String, BTreeMap<String, String>), String> {
    if entries.is_empty() {
        return Err("no asset entries".to_string());
    }

    // Map mason targets to our triples; drop unknown targets silently.
    let mapped: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(target, file)| {
            mason_target_to_triple(target).map(|triple| (triple.to_string(), file.clone()))
        })
        .collect();

    if mapped.is_empty() {
        return Err("no recognized triples".to_string());
    }

    // Try to find a common pattern by substituting each triple with `{triple}`.
    // After substitution, all filenames must collapse to the same template.
    let patterns: Vec<String> = mapped
        .iter()
        .map(|(triple, file)| file.replace(triple.as_str(), "{triple}"))
        .collect();

    let first = &patterns[0];
    if !patterns.iter().all(|p| p == first) {
        return Err("asset format varies per triple".to_string());
    }

    // Build sha256 placeholder map (empty strings — real checksums require
    // network fetches per-asset which we intentionally skip here).
    let sha256: BTreeMap<String, String> = mapped
        .into_iter()
        .map(|(triple, _)| (triple, String::new()))
        .collect();

    Ok((first.clone(), sha256))
}

// ── Translation ──────────────────────────────────────────────────────────────

/// Result of translating one mason package.
#[derive(Debug)]
pub struct TranslatedTool {
    pub name: String,
    pub description: String,
    pub version: String,
    pub bin: String,
    pub category: String, // "lsp" | "formatter" | "linter"
    pub method: TranslatedMethod,
}

#[derive(Debug)]
pub enum TranslatedMethod {
    Cargo {
        crate_name: String,
    },
    Npm {
        package: String,
    },
    Pip {
        package: String,
    },
    GoInstall {
        module: String,
    },
    Github {
        repo: String,
        asset_pattern: String,
        sha256: BTreeMap<String, String>,
    },
}

/// Translate a mason category list to our single category string.
/// Returns `None` if none of the categories map to a known one.
///
/// DAP is translated here but filtered out in the emit loop (v1 skip).
fn translate_category(cats: &Value) -> Option<String> {
    let arr = cats.as_array()?;
    for cat in arr {
        let s = cat.as_str()?;
        match s {
            "LSP" => return Some("lsp".to_string()),
            "Formatter" => return Some("formatter".to_string()),
            "Linter" => return Some("linter".to_string()),
            "DAP" => return Some("dap".to_string()),
            _ => {}
        }
    }
    None
}

/// Parse `pkg:cargo/foo@1.2.3` style source IDs.
/// Returns `(scheme, path, version)` or `None`.
fn parse_purl(id: &str) -> Option<(&str, &str, &str)> {
    // Format: `pkg:<type>/<path>@<version>`
    let rest = id.strip_prefix("pkg:")?;
    let (scheme, rest) = rest.split_once('/')?;
    let (path, version) = rest.split_once('@')?;
    Some((scheme, path, version))
}

/// Attempt to pick the binary name from mason's `bin` block.
///
/// Mason `bin` can be:
///   - absent / null
///   - a string `"<bin_name>"`
///   - an object `{ "<bin_name>": "<path>" }` or `{ "<bin_name>": [...] }`
///
/// We prefer the key that equals the package name; otherwise take the first key.
fn pick_bin(bin_val: &Value, pkg_name: &str) -> Option<String> {
    match bin_val {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Object(map) => {
            if map.is_empty() {
                return None;
            }
            // Prefer the key matching package name; else first key.
            let key = if map.contains_key(pkg_name) {
                pkg_name
            } else {
                map.keys().next().map(String::as_str)?
            };
            Some(key.to_string())
        }
        _ => None,
    }
}

/// Collect per-target asset entries from a mason `source` block.
///
/// Mason source asset may be:
///   - absent / null
///   - a string (single file, no per-target distinction)
///   - an array of objects `[{ target: "linux_x64", file: "foo.tar.gz" }, ...]`
///   - an array of strings
fn collect_asset_entries(source: &Value) -> Vec<(String, String)> {
    let asset = &source["asset"];
    match asset {
        Value::String(_) | Value::Null => vec![],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|entry| {
                let target = entry["target"].as_str()?.to_string();
                let file = entry["file"].as_str()?.to_string();
                Some((target, file))
            })
            .collect(),
        _ => vec![],
    }
}

/// Translate a single mason package JSON object.
///
/// Returns `Ok(TranslatedTool)` on success, or `Err(reason)` for skip.
pub fn translate_package(pkg: &Value) -> Result<TranslatedTool, String> {
    let name = pkg["name"].as_str().ok_or("missing name")?.to_string();

    // Name filter: lowercase ASCII alnum + '-' + '_'
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!("invalid tool name: {name:?}"));
    }

    let description = pkg["description"].as_str().unwrap_or("").to_string();

    // Category translation — filter out DAP and unmapped
    let category = translate_category(&pkg["categories"])
        .ok_or_else(|| "no supported category (not LSP/Formatter/Linter)".to_string())?;

    // Binary name
    let bin_val = &pkg["bin"];
    let bin = pick_bin(bin_val, &name).ok_or_else(|| "no bin entry".to_string())?;

    if bin.is_empty() {
        return Err("empty bin".to_string());
    }

    // Source block
    let source = &pkg["source"];
    let source_id = source["id"].as_str().ok_or("missing source.id")?;

    let (scheme, path, version) =
        parse_purl(source_id).ok_or_else(|| format!("unparseable source id: {source_id:?}"))?;

    if version.is_empty() {
        return Err("empty version in purl".to_string());
    }

    let method = match scheme {
        "cargo" => TranslatedMethod::Cargo {
            crate_name: path.to_string(),
        },
        "npm" => {
            // npm packages may start with @scope/pkg; `path` already has it.
            TranslatedMethod::Npm {
                package: path.to_string(),
            }
        }
        "pypi" => TranslatedMethod::Pip {
            package: path.to_string(),
        },
        "golang" => TranslatedMethod::GoInstall {
            module: path.to_string(),
        },
        "github" => {
            // `path` is `owner/repo`
            let repo = path.to_string();
            let entries = collect_asset_entries(source);

            if entries.is_empty() {
                // Single-asset (no per-target entries) — try source["asset"]
                let asset_str = source["asset"].as_str();
                let pattern = asset_str
                    .map(|s| s.to_string())
                    .ok_or_else(|| "github: no asset entries and no asset string".to_string())?;
                if pattern.is_empty() {
                    return Err("github: empty asset pattern".to_string());
                }
                // No per-target sha256 available.
                let sha256 = BTreeMap::new();
                TranslatedMethod::Github {
                    repo,
                    asset_pattern: pattern,
                    sha256,
                }
            } else {
                let (asset_pattern, sha256) = infer_asset_pattern(&entries)
                    .map_err(|e| format!("github asset pattern: {e}"))?;
                TranslatedMethod::Github {
                    repo,
                    asset_pattern,
                    sha256,
                }
            }
        }
        "generic" => return Err("generic source: too varied (curl-then-script)".to_string()),
        other => return Err(format!("unsupported source scheme: {other}")),
    };

    Ok(TranslatedTool {
        name,
        description,
        version: version.to_string(),
        bin,
        category,
        method,
    })
}

// ── TOML emission ─────────────────────────────────────────────────────────────

/// Hand-roll deterministic TOML output.
///
/// `toml::to_string_pretty` does not guarantee field order when serializing
/// mixed flat/table structs, so we write it manually for a stable diff.
fn emit_toml(tools: &BTreeMap<String, TranslatedTool>, pin: &str) -> String {
    let mut out = String::new();

    out.push_str("[meta]\n");
    out.push_str("schema_version = 1\n");
    out.push_str(&format!("upstream_rev = \"{pin}\"\n"));

    for (name, tool) in tools {
        out.push('\n');
        out.push_str(&format!("[tool.{name}]\n"));
        out.push_str(&format!("category = \"{}\"\n", tool.category));
        // Escape description: replace `\` and `"` in the string value.
        let desc_escaped = tool.description.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str(&format!("description = \"{desc_escaped}\"\n"));
        out.push_str(&format!("version = \"{}\"\n", tool.version));
        out.push_str(&format!("bin = \"{}\"\n", tool.bin));

        match &tool.method {
            TranslatedMethod::Cargo { crate_name } => {
                out.push_str("method = \"cargo\"\n");
                out.push_str(&format!("crate_name = \"{crate_name}\"\n"));
            }
            TranslatedMethod::Npm { package } => {
                out.push_str("method = \"npm\"\n");
                out.push_str(&format!("package = \"{package}\"\n"));
            }
            TranslatedMethod::Pip { package } => {
                out.push_str("method = \"pip\"\n");
                out.push_str(&format!("package = \"{package}\"\n"));
            }
            TranslatedMethod::GoInstall { module } => {
                out.push_str("method = \"goinstall\"\n");
                out.push_str(&format!("module = \"{module}\"\n"));
            }
            TranslatedMethod::Github {
                repo,
                asset_pattern,
                sha256,
            } => {
                out.push_str("method = \"github\"\n");
                out.push_str(&format!("repo = \"{repo}\"\n"));
                out.push_str(&format!("asset_pattern = \"{asset_pattern}\"\n"));
                out.push_str(&format!("[tool.{name}.sha256]\n"));
                for (triple, hash) in sha256 {
                    out.push_str(&format!("\"{triple}\" = \"{hash}\"\n"));
                }
            }
        }
    }

    out
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let pin = &cli.pin;
    let url = format!(
        "https://github.com/mason-org/mason-registry/releases/download/{pin}/registry.json.zip"
    );

    eprintln!("downloading {url}");

    // Download the zip.
    let response = reqwest::blocking::get(&url)?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("404: release tag {pin:?} not found at {url}");
    }
    if !status.is_success() {
        anyhow::bail!("HTTP {status} fetching {url}");
    }

    let bytes = response.bytes()?;
    eprintln!("downloaded {} bytes", bytes.len());

    // Unzip in-memory.
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    // Find registry.json inside the zip.
    let json_src = {
        let mut found = None;
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if file.name() == "registry.json" {
                found = Some(i);
                break;
            }
        }
        let idx = found.ok_or_else(|| anyhow::anyhow!("registry.json not found in zip"))?;
        let mut file = archive.by_index(idx)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        buf
    };

    eprintln!("parsing registry.json ({} chars)", json_src.len());
    let registry: Value = serde_json::from_str(&json_src)?;

    // Walk every package.
    let packages = registry
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("registry.json root is not an array"))?;

    eprintln!("found {} packages in upstream", packages.len());
    let total = packages.len();

    let mut translated: BTreeMap<String, TranslatedTool> = BTreeMap::new();
    let mut skip_count = 0usize;
    let mut skip_reasons: BTreeMap<String, usize> = BTreeMap::new();

    for pkg in packages {
        match translate_package(pkg) {
            Ok(tool) => {
                // Filter: only Lsp, Formatter, Linter (skip Dap for v1)
                if tool.category == "dap" {
                    let reason = "category=dap (v1 skip)".to_string();
                    *skip_reasons.entry(reason).or_insert(0) += 1;
                    skip_count += 1;
                    continue;
                }

                // Github entries with empty sha256 get a warning but are still
                // emitted — the installer will require --insecure or explicit
                // acknowledgement (out of scope here).
                if let TranslatedMethod::Github { sha256, .. } = &tool.method
                    && sha256.is_empty()
                {
                    eprintln!(
                        "warning: {} (github): no sha256 checksums; emitting with empty map",
                        tool.name
                    );
                }

                translated.insert(tool.name.clone(), tool);
            }
            Err(reason) => {
                *skip_reasons.entry(reason).or_insert(0) += 1;
                skip_count += 1;
            }
        }
    }

    let translated_count = translated.len();

    // Summary
    eprintln!(
        "\n{translated_count} tools translated, {skip_count} skipped, {total} total in upstream"
    );
    // Top skip reasons
    let mut reasons_sorted: Vec<_> = skip_reasons.iter().collect();
    reasons_sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, count) in reasons_sorted.iter().take(10) {
        eprintln!("  skip({count}): {reason}");
    }

    let toml = emit_toml(&translated, pin);

    if cli.dry_run {
        println!("{toml}");
        return Ok(());
    }

    // Determine output path.
    let out_path = cli.out.unwrap_or_else(|| {
        // Default: `crates/hjkl-anvil/anvil.toml` relative to the workspace root.
        // We walk up from the binary location — but since this runs via `cargo run`,
        // CARGO_MANIFEST_DIR is set.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest_dir).join("anvil.toml")
    });

    std::fs::write(&out_path, &toml)?;
    eprintln!("wrote {} ({translated_count} tools)", out_path.display());

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Translation unit tests ────────────────────────────────────────────────

    fn make_pkg(
        name: &str,
        categories: &[&str],
        source_id: &str,
        bin_name: &str,
    ) -> serde_json::Value {
        let cats: Vec<Value> = categories
            .iter()
            .map(|c| Value::String(c.to_string()))
            .collect();
        let mut bin_obj = serde_json::Map::new();
        if !bin_name.is_empty() {
            bin_obj.insert(
                bin_name.to_string(),
                Value::String(format!("bin/{bin_name}")),
            );
        }
        serde_json::json!({
            "name": name,
            "description": "A test tool",
            "categories": cats,
            "source": {
                "id": source_id,
            },
            "bin": Value::Object(bin_obj),
        })
    }

    #[test]
    fn translate_cargo_package() {
        let pkg = make_pkg("taplo", &["LSP"], "pkg:cargo/taplo-cli@0.9.3", "taplo");
        let tool = translate_package(&pkg).expect("should translate");
        assert_eq!(tool.name, "taplo");
        assert_eq!(tool.category, "lsp");
        assert_eq!(tool.version, "0.9.3");
        assert_eq!(tool.bin, "taplo");
        assert!(
            matches!(tool.method, TranslatedMethod::Cargo { ref crate_name } if crate_name == "taplo-cli")
        );
    }

    #[test]
    fn translate_npm_package() {
        let pkg = make_pkg(
            "pyright",
            &["LSP"],
            "pkg:npm/pyright@1.1.395",
            "pyright-langserver",
        );
        let tool = translate_package(&pkg).expect("should translate");
        assert_eq!(tool.category, "lsp");
        assert!(
            matches!(tool.method, TranslatedMethod::Npm { ref package } if package == "pyright")
        );
    }

    #[test]
    fn translate_pypi_package() {
        let pkg = make_pkg("black", &["Formatter"], "pkg:pypi/black@24.0.0", "black");
        let tool = translate_package(&pkg).expect("should translate");
        assert_eq!(tool.category, "formatter");
        assert!(matches!(tool.method, TranslatedMethod::Pip { ref package } if package == "black"));
    }

    #[test]
    fn translate_golang_package() {
        let pkg = make_pkg(
            "gopls",
            &["LSP"],
            "pkg:golang/golang.org/x/tools/gopls@v0.17.1",
            "gopls",
        );
        let tool = translate_package(&pkg).expect("should translate");
        assert_eq!(tool.category, "lsp");
        assert!(
            matches!(tool.method, TranslatedMethod::GoInstall { ref module } if module == "golang.org/x/tools/gopls")
        );
    }

    #[test]
    fn translate_github_package_per_target() {
        let pkg = serde_json::json!({
            "name": "rust-analyzer",
            "description": "Rust language server",
            "categories": ["LSP"],
            "source": {
                "id": "pkg:github/rust-lang/rust-analyzer@2025-01-13",
                "asset": [
                    { "target": "linux_x64_gnu", "file": "rust-analyzer-x86_64-unknown-linux-gnu.gz" },
                    { "target": "linux_arm64_gnu", "file": "rust-analyzer-aarch64-unknown-linux-gnu.gz" },
                    { "target": "darwin_arm64", "file": "rust-analyzer-aarch64-apple-darwin.gz" },
                ],
            },
            "bin": { "rust-analyzer": "bin/rust-analyzer" },
        });
        let tool = translate_package(&pkg).expect("should translate");
        assert_eq!(tool.name, "rust-analyzer");
        if let TranslatedMethod::Github {
            asset_pattern,
            sha256,
            ..
        } = &tool.method
        {
            assert_eq!(asset_pattern, "rust-analyzer-{triple}.gz");
            // sha256 keys present but values are empty (no upstream checksums)
            assert!(sha256.contains_key("x86_64-unknown-linux-gnu"));
            assert!(sha256.contains_key("aarch64-unknown-linux-gnu"));
            assert!(sha256.contains_key("aarch64-apple-darwin"));
        } else {
            panic!("expected Github method");
        }
    }

    #[test]
    fn translate_generic_package_skipped() {
        let pkg = make_pkg(
            "some-tool",
            &["LSP"],
            "pkg:generic/some-tool@1.0.0",
            "some-tool",
        );
        let err = translate_package(&pkg).expect_err("generic should be skipped");
        assert!(err.contains("generic"), "{err}");
    }

    #[test]
    fn translate_unsupported_scheme_skipped() {
        let pkg = make_pkg(
            "some-gem",
            &["Formatter"],
            "pkg:gem/some-gem@1.0.0",
            "some-gem",
        );
        let err = translate_package(&pkg).expect_err("gem should be skipped");
        assert!(err.contains("unsupported source scheme"), "{err}");
    }

    #[test]
    fn translate_no_matching_category_skipped() {
        // Only DAP category — should be translated but category="dap"
        let pkg = make_pkg("some-dap", &["DAP"], "pkg:cargo/some-dap@1.0.0", "some-dap");
        // DAP packages do translate (not skipped at translate_package level),
        // they are filtered out afterwards in the main loop.
        let tool = translate_package(&pkg);
        // Translate may succeed but category is "dap" (if DAP mapped)
        // OR fail if no supported category — per spec DAP is translated but
        // filtered out in the emit loop.
        match tool {
            Ok(t) => assert_eq!(t.category, "dap"),
            Err(e) => assert!(e.contains("no supported category"), "{e}"),
        }
    }

    #[test]
    fn translate_unknown_category_skipped() {
        let pkg = make_pkg(
            "some-tool",
            &["UnknownCategory"],
            "pkg:cargo/some-tool@1.0.0",
            "some-tool",
        );
        let err = translate_package(&pkg).expect_err("unknown category should skip");
        assert!(err.contains("no supported category"), "{err}");
    }

    // ── Asset-pattern inference tests ─────────────────────────────────────────

    #[test]
    fn infer_pattern_common_triple_substitution() {
        let entries = vec![
            (
                "linux_x64_gnu".to_string(),
                "rust-analyzer-x86_64-unknown-linux-gnu.gz".to_string(),
            ),
            (
                "linux_arm64_gnu".to_string(),
                "rust-analyzer-aarch64-unknown-linux-gnu.gz".to_string(),
            ),
            (
                "darwin_arm64".to_string(),
                "rust-analyzer-aarch64-apple-darwin.gz".to_string(),
            ),
        ];
        let (pattern, sha256) = infer_asset_pattern(&entries).expect("should infer pattern");
        assert_eq!(pattern, "rust-analyzer-{triple}.gz");
        assert_eq!(sha256.len(), 3);
        assert!(sha256.contains_key("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn infer_pattern_varies_per_triple_skipped() {
        // Different extensions per platform — cannot unify.
        let entries = vec![
            (
                "linux_x64_gnu".to_string(),
                "tool-x86_64-unknown-linux-gnu.tar.gz".to_string(),
            ),
            (
                "darwin_arm64".to_string(),
                "tool-aarch64-apple-darwin.zip".to_string(),
            ),
        ];
        let err = infer_asset_pattern(&entries).expect_err("should fail");
        assert!(err.contains("varies"), "{err}");
    }

    #[test]
    fn infer_pattern_no_entries() {
        let err = infer_asset_pattern(&[]).expect_err("should fail on empty");
        assert!(!err.is_empty());
    }

    // ── Round-trip test ───────────────────────────────────────────────────────

    #[test]
    fn roundtrip_three_tools() {
        let mut tools: BTreeMap<String, TranslatedTool> = BTreeMap::new();

        tools.insert(
            "black".to_string(),
            TranslatedTool {
                name: "black".to_string(),
                description: "Python formatter".to_string(),
                version: "24.0.0".to_string(),
                bin: "black".to_string(),
                category: "formatter".to_string(),
                method: TranslatedMethod::Pip {
                    package: "black".to_string(),
                },
            },
        );

        tools.insert(
            "gopls".to_string(),
            TranslatedTool {
                name: "gopls".to_string(),
                description: "Go language server".to_string(),
                version: "v0.17.1".to_string(),
                bin: "gopls".to_string(),
                category: "lsp".to_string(),
                method: TranslatedMethod::GoInstall {
                    module: "golang.org/x/tools/gopls".to_string(),
                },
            },
        );

        tools.insert(
            "taplo".to_string(),
            TranslatedTool {
                name: "taplo".to_string(),
                description: "TOML language server".to_string(),
                version: "0.9.3".to_string(),
                bin: "taplo".to_string(),
                category: "lsp".to_string(),
                method: TranslatedMethod::Cargo {
                    crate_name: "taplo-cli".to_string(),
                },
            },
        );

        let toml_str = emit_toml(&tools, "test-pin");

        // Parse back via manifest::parse_str
        let manifest = hjkl_anvil::manifest::parse_str(&toml_str).expect("emitted TOML must parse");

        assert_eq!(manifest.tool.len(), 3);
        assert!(manifest.tool.contains_key("black"));
        assert!(manifest.tool.contains_key("gopls"));
        assert!(manifest.tool.contains_key("taplo"));

        assert_eq!(manifest.meta.upstream_rev, "test-pin");
        assert_eq!(manifest.meta.schema_version, 1);

        // Check methods round-trip
        use hjkl_anvil::manifest::InstallMethod;
        assert!(matches!(
            manifest.tool["black"].method,
            InstallMethod::Pip(_)
        ));
        assert!(matches!(
            manifest.tool["gopls"].method,
            InstallMethod::GoInstall(_)
        ));
        assert!(matches!(
            manifest.tool["taplo"].method,
            InstallMethod::Cargo(_)
        ));

        // Validate passes (no github entries with empty sha256 here)
        manifest
            .validate()
            .expect("roundtrip manifest must validate");
    }
}
