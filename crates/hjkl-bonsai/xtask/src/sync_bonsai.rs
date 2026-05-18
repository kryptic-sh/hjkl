//! `cargo xtask sync-bonsai` — regenerate `../bonsai.toml`.
//!
//! Pulls language data from:
//!   - helix-editor/helix/languages.toml
//!   - nvim-treesitter/nvim-treesitter/lockfile.json
//!   - nvim-treesitter/nvim-treesitter/lua/nvim-treesitter/parsers.lua
//!
//! Merges by lowercase language name, prefers nvim-treesitter for
//! `git_url + git_rev` (updates more frequently), takes extensions union,
//! records provenance in `source`.
//!
//! `query_source` is derived from merge provenance: helix-only → Helix,
//! nvim-only or both → NvimTreesitter (nvim-treesitter has more comprehensive
//! queries for shared grammars).
//!
//! URLs, git revs, and file paths are facts (not copyrightable). Format
//! and aggregation logic are MIT (this xtask).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::Deserialize;

const HELIX_LANGUAGES_TOML: &str =
    "https://raw.githubusercontent.com/helix-editor/helix/master/languages.toml";
const NVIM_LOCKFILE_JSON: &str =
    "https://raw.githubusercontent.com/nvim-treesitter/nvim-treesitter/master/lockfile.json";
const NVIM_PARSERS_LUA: &str = "https://raw.githubusercontent.com/nvim-treesitter/nvim-treesitter/master/lua/nvim-treesitter/parsers.lua";

const HELIX_REPO: &str = "https://github.com/helix-editor/helix";
const NVIM_REPO: &str = "https://github.com/nvim-treesitter/nvim-treesitter";

const GITHUB_API: &str = "https://api.github.com/repos";

pub fn run(_args: &[String]) -> Result<()> {
    eprintln!("fetching {HELIX_LANGUAGES_TOML}");
    let helix_src = http_get(HELIX_LANGUAGES_TOML)?;
    let helix = parse_helix(&helix_src)?;
    eprintln!("helix: {} languages", helix.len());

    eprintln!("fetching {NVIM_PARSERS_LUA}");
    let nvim_lua = http_get(NVIM_PARSERS_LUA)?;
    let mut nvim = parse_nvim_lua(&nvim_lua)?;
    eprintln!("nvim-treesitter parsers.lua: {} languages", nvim.len());

    eprintln!("fetching {NVIM_LOCKFILE_JSON}");
    let lock_src = http_get(NVIM_LOCKFILE_JSON)?;
    apply_lockfile(&mut nvim, &lock_src)?;

    let merged = merge(&helix, &nvim);
    eprintln!("merged: {} languages", merged.len());

    // Fetch current HEAD SHAs for the two query-source repos.
    eprintln!("fetching helix HEAD SHA");
    let helix_rev = fetch_head_sha("helix-editor", "helix")?;
    eprintln!("helix HEAD: {helix_rev}");

    eprintln!("fetching nvim-treesitter HEAD SHA");
    let nvim_rev = fetch_head_sha("nvim-treesitter", "nvim-treesitter")?;
    eprintln!("nvim-treesitter HEAD: {nvim_rev}");

    let toml = emit_toml(&merged, &helix_rev, &nvim_rev);
    let dest = manifest_path()?;
    fs::write(&dest, toml).with_context(|| format!("write {}", dest.display()))?;
    eprintln!("wrote {}", dest.display());
    Ok(())
}

fn fetch_head_sha(owner: &str, repo: &str) -> Result<String> {
    let url = format!("{GITHUB_API}/{owner}/{repo}/git/refs/heads/master");
    let body = http_get(&url)?;
    // Parse {"ref":…,"object":{"sha":"…","type":"commit","url":"…"},"url":…}
    let v: serde_json::Value = serde_json::from_str(&body).context("parse GitHub refs JSON")?;
    v["object"]["sha"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("missing object.sha in {url}"))
}

fn http_get(url: &str) -> Result<String> {
    let body = ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()
        .with_context(|| format!("http get {url}"))?
        .body_mut()
        .read_to_string()?;
    Ok(body)
}

fn manifest_path() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest
        .parent()
        .ok_or_else(|| anyhow!("xtask manifest dir has no parent"))?
        .join("bonsai.toml"))
}

// ----------------------------------------------------------------------------
// Manifest entry shape
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Entry {
    git_url: String,
    git_rev: String,
    subpath: Option<String>,
    extensions: BTreeSet<String>,
    c_files: Vec<String>,
    source: String,
    query_source: QuerySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuerySource {
    Helix,
    NvimTreesitter,
}

// ----------------------------------------------------------------------------
// Helix
// ----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HelixDoc {
    #[serde(default, rename = "language")]
    languages: Vec<HelixLanguage>,
    #[serde(default, rename = "grammar")]
    grammars: Vec<HelixGrammar>,
}

#[derive(Debug, Deserialize)]
struct HelixLanguage {
    name: String,
    grammar: Option<String>,
    #[serde(default, rename = "file-types")]
    file_types: Vec<HelixFileType>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum HelixFileType {
    Bare(String),
    #[allow(dead_code)]
    Map {
        #[serde(default)]
        suffix: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct HelixGrammar {
    name: String,
    source: HelixSource,
}

#[derive(Debug, Deserialize)]
struct HelixSource {
    git: Option<String>,
    rev: Option<String>,
    subpath: Option<String>,
}

fn parse_helix(toml_src: &str) -> Result<BTreeMap<String, Entry>> {
    let doc: HelixDoc = toml::from_str(toml_src).context("parse helix languages.toml")?;
    let mut grammar_by_name: BTreeMap<String, &HelixGrammar> = BTreeMap::new();
    for g in &doc.grammars {
        if g.source.git.is_some() && g.source.rev.is_some() {
            grammar_by_name.insert(g.name.to_lowercase(), g);
        }
    }
    let mut out = BTreeMap::new();
    for lang in &doc.languages {
        let grammar_name = lang
            .grammar
            .clone()
            .unwrap_or_else(|| lang.name.clone())
            .to_lowercase();
        let Some(g) = grammar_by_name.get(grammar_name.as_str()) else {
            continue;
        };
        let mut extensions = BTreeSet::new();
        for ft in &lang.file_types {
            if let HelixFileType::Bare(s) = ft {
                let s = s.trim_start_matches('.');
                if !s.is_empty() {
                    extensions.insert(s.to_string());
                }
            }
        }
        if extensions.is_empty() {
            continue;
        }
        let entry = Entry {
            git_url: g
                .source
                .git
                .clone()
                .unwrap()
                .trim_end_matches('/')
                .trim_end_matches(".git")
                .to_string(),
            git_rev: g.source.rev.clone().unwrap(),
            subpath: g.source.subpath.clone(),
            extensions,
            c_files: vec!["src/parser.c".into(), "src/scanner.c".into()],
            source: "helix".into(),
            query_source: QuerySource::Helix,
        };
        out.insert(lang.name.to_lowercase(), entry);
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// nvim-treesitter
// ----------------------------------------------------------------------------

fn parse_nvim_lua(lua: &str) -> Result<BTreeMap<String, Entry>> {
    let url_re = Regex::new(r#"url\s*=\s*"([^"]+)""#)?;
    let files_re = Regex::new(r"(?s)files\s*=\s*\{([^{}]*)\}")?;
    let location_re = Regex::new(r#"location\s*=\s*"([^"]+)""#)?;
    let requires_gen_re = Regex::new(r"requires_generate_from_grammar\s*=\s*true")?;
    let filetype_re = Regex::new(r#"\bfiletype\s*=\s*"([^"]+)""#)?;
    let filetypes_re = Regex::new(r"(?s)\bfiletypes\s*=\s*\{([^{}]*)\}")?;
    let install_info_re =
        Regex::new(r"(?s)install_info\s*=\s*\{(?P<install>(?:[^{}]|\{[^{}]*\})*?)\}")?;
    let entry_header_re = Regex::new(r"\blist\.([a-zA-Z0-9_]+)\s*=\s*\{")?;

    let bytes = lua.as_bytes();
    let mut out = BTreeMap::new();
    for hdr in entry_header_re.captures_iter(lua) {
        let name = hdr.get(1).unwrap().as_str().to_lowercase();
        let body_start = hdr.get(0).unwrap().end();
        let Some(body_end) = match_close(bytes, body_start - 1) else {
            continue;
        };
        let body = &lua[body_start..body_end];

        let Some(install_m) = install_info_re.captures(body) else {
            continue;
        };
        let install = install_m.name("install").unwrap().as_str();

        if requires_gen_re.is_match(install) {
            continue;
        }
        let Some(url_m) = url_re.captures(install) else {
            continue;
        };
        let url = url_m
            .get(1)
            .unwrap()
            .as_str()
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_string();

        let c_files = if let Some(f) = files_re.captures(install) {
            f.get(1)
                .unwrap()
                .as_str()
                .split(',')
                .filter_map(|s| {
                    let s = s.trim().trim_matches('"');
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                })
                .collect()
        } else {
            vec!["src/parser.c".to_string()]
        };
        if c_files.is_empty() {
            continue;
        }

        let subpath = location_re
            .captures(install)
            .map(|m| m.get(1).unwrap().as_str().to_string());

        let mut extensions = BTreeSet::new();
        if let Some(list) = filetypes_re.captures(body) {
            for tok in list.get(1).unwrap().as_str().split(',') {
                let t = tok.trim().trim_matches('"').trim_start_matches('.');
                if !t.is_empty() {
                    extensions.insert(t.to_string());
                }
            }
        } else if let Some(single) = filetype_re.captures(body) {
            let t = single.get(1).unwrap().as_str().trim_start_matches('.');
            if !t.is_empty() {
                extensions.insert(t.to_string());
            }
        } else {
            extensions.insert(name.clone());
        }
        if extensions.is_empty() {
            continue;
        }

        out.insert(
            name,
            Entry {
                git_url: url,
                git_rev: String::new(),
                subpath,
                extensions,
                c_files,
                source: "nvim-treesitter".into(),
                query_source: QuerySource::NvimTreesitter,
            },
        );
    }
    Ok(out)
}

fn match_close(bytes: &[u8], open: usize) -> Option<usize> {
    debug_assert_eq!(bytes.get(open).copied(), Some(b'{'));
    let mut depth: i32 = 0;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[derive(Debug, Deserialize)]
struct LockEntry {
    revision: String,
}

fn apply_lockfile(grammars: &mut BTreeMap<String, Entry>, lock_src: &str) -> Result<()> {
    let lock: BTreeMap<String, LockEntry> =
        serde_json::from_str(lock_src).context("parse lockfile.json")?;
    for (name, entry) in grammars.iter_mut() {
        if let Some(le) = lock.get(name) {
            entry.git_rev = le.revision.clone();
        }
    }
    Ok(())
}

// ----------------------------------------------------------------------------
// Merge
// ----------------------------------------------------------------------------

fn merge(
    helix: &BTreeMap<String, Entry>,
    nvim: &BTreeMap<String, Entry>,
) -> BTreeMap<String, Entry> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(helix.keys().cloned());
    names.extend(nvim.keys().cloned());

    let mut out = BTreeMap::new();
    for name in names {
        let h = helix.get(&name);
        let n = nvim.get(&name);
        // Both → nvim base (better queries); helix-only → helix; nvim-only → nvim.
        let entry = match (h, n) {
            (Some(h), Some(n)) => {
                let mut e = n.clone();
                e.extensions.extend(h.extensions.iter().cloned());
                e.source = "helix+nvim-treesitter".into();
                e.query_source = QuerySource::NvimTreesitter;
                e
            }
            (None, Some(n)) => n.clone(),
            (Some(h), None) => h.clone(),
            (None, None) => unreachable!(),
        };
        if entry.git_rev.is_empty() {
            continue;
        }
        if entry.extensions.is_empty() {
            continue;
        }
        out.insert(name, entry);
    }
    out
}

// ----------------------------------------------------------------------------
// Emit
// ----------------------------------------------------------------------------

fn emit_toml(grammars: &BTreeMap<String, Entry>, helix_rev: &str, nvim_rev: &str) -> String {
    let mut s = String::new();
    s.push_str("# bonsai.toml — language manifest for hjkl-bonsai.\n");
    s.push_str("#\n");
    s.push_str("# Generated by `cargo xtask sync-bonsai` from:\n");
    s.push_str("#   - helix-editor/helix/languages.toml\n");
    s.push_str("#   - nvim-treesitter/nvim-treesitter/{lockfile.json, parsers.lua}\n");
    s.push_str("#\n");
    s.push_str("# URLs, git revs, and file paths are facts (not copyrightable).\n");
    s.push_str("# Re-generate via: cargo xtask sync-bonsai\n\n");

    // [meta] block
    s.push_str("[meta]\n");
    s.push_str(&format!("helix_repo = \"{HELIX_REPO}\"\n"));
    s.push_str(&format!("helix_rev = \"{helix_rev}\"\n"));
    s.push_str(&format!("nvim_treesitter_repo = \"{NVIM_REPO}\"\n"));
    s.push_str(&format!("nvim_treesitter_rev = \"{nvim_rev}\"\n"));
    s.push('\n');

    for (name, g) in grammars {
        let qs = match g.query_source {
            QuerySource::Helix => "helix",
            QuerySource::NvimTreesitter => "nvim_treesitter",
        };
        s.push_str(&format!("[language.{name}]\n"));
        s.push_str(&format!("git_url = \"{}\"\n", g.git_url));
        s.push_str(&format!("git_rev = \"{}\"\n", g.git_rev));
        if let Some(sp) = &g.subpath {
            s.push_str(&format!("subpath = \"{sp}\"\n"));
        }
        let ext = g
            .extensions
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!("extensions = [{ext}]\n"));
        let cf = g
            .c_files
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        s.push_str(&format!("c_files = [{cf}]\n"));
        s.push_str(&format!("query_source = \"{qs}\"\n"));
        s.push_str(&format!("source = \"{}\"\n", g.source));
        s.push('\n');
    }
    s
}

// ----------------------------------------------------------------------------
// Tests (canned fixtures, no network)
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_helix_toml() -> &'static str {
        r#"
[[grammar]]
name = "rust"
[grammar.source]
git = "https://github.com/tree-sitter/tree-sitter-rust"
rev = "aaaa0000bbbb"

[[grammar]]
name = "python"
[grammar.source]
git = "https://github.com/tree-sitter/tree-sitter-python"
rev = "bbbb1111cccc"

[[language]]
name = "rust"
file-types = ["rs"]

[[language]]
name = "python"
file-types = ["py"]
"#
    }

    fn fake_nvim_lua() -> &'static str {
        r#"
list.go = {
  install_info = {
    url = "https://github.com/tree-sitter/tree-sitter-go",
    files = { "src/parser.c" },
  },
  filetype = "go",
}

list.python = {
  install_info = {
    url = "https://github.com/tree-sitter/tree-sitter-python",
    files = { "src/parser.c" },
  },
  filetype = "py",
}
"#
    }

    fn fake_lockfile() -> &'static str {
        r#"{"go": {"revision": "dddd4444eeee"}, "python": {"revision": "cccc2222ffff"}}"#
    }

    #[test]
    fn parse_helix_extracts_entries() {
        let out = parse_helix(fake_helix_toml()).unwrap();
        assert!(out.contains_key("rust"), "rust missing");
        assert!(out.contains_key("python"), "python missing");
        assert_eq!(out["rust"].git_rev, "aaaa0000bbbb");
    }

    #[test]
    fn parse_helix_sets_helix_query_source() {
        let out = parse_helix(fake_helix_toml()).unwrap();
        assert_eq!(out["rust"].query_source, QuerySource::Helix);
        assert_eq!(out["python"].query_source, QuerySource::Helix);
    }

    #[test]
    fn parse_nvim_lua_extracts_entries() {
        let out = parse_nvim_lua(fake_nvim_lua()).unwrap();
        assert!(out.contains_key("go"), "go missing");
    }

    #[test]
    fn parse_nvim_lua_sets_nvim_query_source() {
        let out = parse_nvim_lua(fake_nvim_lua()).unwrap();
        assert_eq!(out["go"].query_source, QuerySource::NvimTreesitter);
    }

    #[test]
    fn apply_lockfile_fills_revs() {
        let mut nvim = parse_nvim_lua(fake_nvim_lua()).unwrap();
        apply_lockfile(&mut nvim, fake_lockfile()).unwrap();
        assert_eq!(nvim["go"].git_rev, "dddd4444eeee");
    }

    #[test]
    fn merge_unions_extensions_and_sets_source() {
        let helix = parse_helix(fake_helix_toml()).unwrap();
        let mut nvim = parse_nvim_lua(fake_nvim_lua()).unwrap();
        apply_lockfile(&mut nvim, fake_lockfile()).unwrap();
        let merged = merge(&helix, &nvim);
        // helix-only
        assert!(merged.contains_key("rust"));
        assert_eq!(merged["rust"].source, "helix");
        // nvim-only
        assert!(merged.contains_key("go"));
        assert_eq!(merged["go"].source, "nvim-treesitter");
    }

    #[test]
    fn merge_provenance_determines_query_source() {
        let helix = parse_helix(fake_helix_toml()).unwrap();
        let mut nvim = parse_nvim_lua(fake_nvim_lua()).unwrap();
        apply_lockfile(&mut nvim, fake_lockfile()).unwrap();
        let merged = merge(&helix, &nvim);

        // helix-only → Helix
        assert_eq!(merged["rust"].query_source, QuerySource::Helix);
        // nvim-only → NvimTreesitter
        assert_eq!(merged["go"].query_source, QuerySource::NvimTreesitter);
        // both → NvimTreesitter
        assert_eq!(merged["python"].query_source, QuerySource::NvimTreesitter);
        assert_eq!(merged["python"].source, "helix+nvim-treesitter");
    }

    #[test]
    fn emit_toml_writes_meta_block() {
        let mut grammars = BTreeMap::new();
        grammars.insert(
            "rust".to_string(),
            Entry {
                git_url: "https://github.com/tree-sitter/tree-sitter-rust".into(),
                git_rev: "aaaa".into(),
                subpath: None,
                extensions: ["rs".into()].into(),
                c_files: vec!["src/parser.c".into()],
                source: "helix".into(),
                query_source: QuerySource::Helix,
            },
        );
        let out = emit_toml(&grammars, "helix-sha-123", "nvim-sha-456");
        assert!(out.contains("[meta]"), "[meta] block missing");
        assert!(
            out.contains("helix_rev = \"helix-sha-123\""),
            "helix_rev missing"
        );
        assert!(
            out.contains("nvim_treesitter_rev = \"nvim-sha-456\""),
            "nvim_rev missing"
        );
        assert!(
            out.contains("query_source = \"helix\""),
            "query_source missing"
        );
        assert!(!out.contains("query_dir"), "old query_dir must not appear");
    }

    #[test]
    fn emitted_toml_parses_back() {
        use hjkl_bonsai::runtime::Manifest;
        let mut grammars = BTreeMap::new();
        grammars.insert(
            "rust".to_string(),
            Entry {
                git_url: "https://github.com/tree-sitter/tree-sitter-rust".into(),
                git_rev: "aaaa0000bbbb1111cccc2222dddd3333eeee4444".into(),
                subpath: None,
                extensions: ["rs".into()].into(),
                c_files: vec!["src/parser.c".into()],
                source: "helix".into(),
                query_source: QuerySource::Helix,
            },
        );
        let toml_str = emit_toml(
            &grammars,
            "aaaa0000bbbb1111cccc2222dddd3333eeee4444",
            "ffff5555aaaa0000bbbb1111cccc2222dddd3333",
        );
        let m = Manifest::from_toml_str(&toml_str).expect("emitted TOML must parse back");
        assert!(m.get("rust").is_some());
        assert_eq!(m.meta.helix_rev, "aaaa0000bbbb1111cccc2222dddd3333eeee4444");
    }
}
