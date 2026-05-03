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

    let toml = emit_toml(&merged);
    let dest = manifest_path()?;
    fs::write(&dest, toml).with_context(|| format!("write {}", dest.display()))?;
    eprintln!("wrote {}", dest.display());
    Ok(())
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
    // xtask runs from xtask/ — bonsai.toml lives one level up.
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
    query_dir: String,
    source: String,
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
            query_dir: "queries".into(),
            source: "helix".into(),
        };
        out.insert(lang.name.to_lowercase(), entry);
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// nvim-treesitter
// ----------------------------------------------------------------------------

fn parse_nvim_lua(lua: &str) -> Result<BTreeMap<String, Entry>> {
    // The file shape is:
    //
    //     list.<name> = {
    //       install_info = { url = "...", files = { ... }, ... },
    //       maintainers = { ... },
    //       filetype = "...",        -- optional
    //       filetypes = { ... },     -- optional
    //     }
    //
    // Use a simple brace-counting scanner to extract each top-level
    // `list.<name> = { ... }` body. Then regex inside that body for
    // the fields we care about.
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
        let body_start = hdr.get(0).unwrap().end(); // just after the `{`
        let Some(body_end) = match_close(bytes, body_start - 1) else {
            continue;
        };
        let body = &lua[body_start..body_end];

        let Some(install_m) = install_info_re.captures(body) else {
            continue;
        };
        let install = install_m.name("install").unwrap().as_str();

        if requires_gen_re.is_match(install) {
            // Skip — needs `tree-sitter generate` (CLI required at install).
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

        // Extensions: prefer filetypes list, fall back to single filetype, else lang name.
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
                git_rev: String::new(), // filled from lockfile
                subpath,
                extensions,
                c_files,
                query_dir: "queries".into(),
                source: "nvim-treesitter".into(),
            },
        );
    }
    Ok(out)
}

/// Given a position in `bytes` pointing at `{`, return the position of
/// the matching close `}`. Skips over nested `{}` pairs. Returns `None`
/// on unbalanced input.
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
            // Skip past Lua string literals so braces inside `"..."` don't confuse us.
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
        let entry = match (h, n) {
            (Some(h), Some(n)) => {
                let mut e = n.clone();
                e.extensions.extend(h.extensions.iter().cloned());
                e.source = "helix+nvim-treesitter".into();
                e
            }
            (None, Some(n)) => n.clone(),
            (Some(h), None) => h.clone(),
            (None, None) => unreachable!(),
        };
        if entry.git_rev.is_empty() {
            // Can't pin reproducibly without a rev — skip.
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

fn emit_toml(grammars: &BTreeMap<String, Entry>) -> String {
    let mut s = String::new();
    s.push_str("# bonsai.toml — language manifest for hjkl-bonsai.\n");
    s.push_str("#\n");
    s.push_str("# Generated by `cargo xtask sync-bonsai` from:\n");
    s.push_str("#   - helix-editor/helix/languages.toml\n");
    s.push_str("#   - nvim-treesitter/nvim-treesitter/{lockfile.json, parsers.lua}\n");
    s.push_str("#\n");
    s.push_str("# URLs, git revs, and file paths are facts (not copyrightable).\n");
    s.push_str("# Re-generate via: cargo xtask sync-bonsai\n\n");
    for (name, g) in grammars {
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
        s.push_str(&format!("query_dir = \"{}\"\n", g.query_dir));
        s.push_str(&format!("source = \"{}\"\n", g.source));
        s.push('\n');
    }
    s
}
