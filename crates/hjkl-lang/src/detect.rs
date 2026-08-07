//! Content-aware filetype detection — the seam every consumer routes its
//! "what language is this file" question through.
//!
//! [`LanguageDirectory::detect`] resolves a file's language name in a fixed
//! precedence order:
//!
//! 1. **Known basename** — files whose *name* is the signal, not an
//!    extension (`Makefile`, `CMakeLists.txt`, `Dockerfile`, `PKGBUILD`, …).
//!    Runs before the extension lookup because a specific basename is a
//!    stronger signal than a catch-all extension claim: the manifest's
//!    `systemd` grammar claims `build`, and `meson.build` must come out as
//!    `meson`, not `systemd`. (vim resolves the same way — its
//!    `filetype.vim` lists `meson.build` before any `*.build` pattern.)
//! 2. **Extension** — the grammar manifest's extension table (`foo.rs` →
//!    `rust`).
//! 3. **Modeline** — a `vim:` / `vi:` / `ex:` modeline `ft=` / `filetype=`
//!    token. Beats a shebang, matching vim: a modeline is an explicit
//!    author-stated type, the shebang a heuristic. (vim lets a modeline beat
//!    the extension too; hjkl's extension step still runs first — the two
//!    rarely disagree, and a `.py` file staying `python` is the less
//!    surprising outcome.)
//! 4. **Shebang** — the interpreter named on the first line
//!    (`#!/usr/bin/env bash` → `bash`). Lowest precedence.
//!
//! The basename and extension steps are cheap and need no content; the
//! modeline and shebang steps read at most the first/last `'modelines'`
//! (default 5) lines, capped at 500 chars each. Callers that only have a
//! path (no content yet) can pass an empty string and get steps 1–2.
//!
//! The tables map to **grammar names in the bonsai manifest** where one
//! exists, and to the closest grammar otherwise (`sh`/`zsh` → `bash`, since
//! hjkl ships no separate `sh`/`zsh` grammar — the bash grammar is the
//! superset). Names that survive this module unvalidated (a modeline
//! `ft=bogus`) are the caller's to honour or reject: `set_language_by_name`
//! resolves unknown names to `Unknown` without attaching anything.

use std::path::Path;

use crate::LanguageDirectory;

/// Options for [`LanguageDirectory::detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectOptions {
    /// Honour `vim:` / `ex:` / `vi:` modeline `ft=` / `filetype=` tokens.
    /// Default `true` (vim's `'modeline'`).
    pub modeline: bool,
    /// Lines scanned at each end of the file for a modeline.
    /// Default `5` (vim's `'modelines'`).
    pub modelines: usize,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            modeline: true,
            modelines: 5,
        }
    }
}

// ── The seam ──────────────────────────────────────────────────────────────────

impl LanguageDirectory {
    /// Resolve `path`'s language name, consulting `content` when the path
    /// alone is not decisive. See the [module docs](self) for the precedence
    /// order. Returns `None` when nothing matches.
    ///
    /// The basename and extension steps return names that are guaranteed to
    /// have a grammar; the shebang and modeline steps return table names that
    /// may not (a modeline `ft=bogus` is returned as-is — attaching it is
    /// the caller's call).
    pub fn detect(&self, path: &Path, content: &str, opts: &DetectOptions) -> Option<String> {
        // 1. Known basename — a specific name beats a catch-all extension
        //    claim (`meson.build` must be `meson`, not `systemd`).
        let basename = path.file_name()?.to_str()?;
        if let Some(name) = known_filename_language(basename) {
            return Some(name.to_string());
        }
        // 2. Extension — the manifest's table (foo.rs → rust).
        if let Some(name) = self.registry.name_for_path(path) {
            return Some(name.to_string());
        }
        // 3. Modeline `ft=` / `filetype=` — beats a shebang (vim parity: an
        //    explicit author-stated type outranks the shebang heuristic).
        if opts.modeline
            && let Some(name) = modeline_filetype(content, opts.modelines)
        {
            return Some(name);
        }
        // 4. Shebang on the first line — lowest precedence.
        if let Some(line) = content.lines().next()
            && let Some(name) = shebang_language(line)
        {
            return Some(name.to_string());
        }
        None
    }
}

// ── Known basenames ───────────────────────────────────────────────────────────

/// Map a file's basename to a language name, for files whose *name* (not
/// extension) is the type signal. Mirrors the classic vim `filetype.vim`
/// `BufNewFile,BufRead` entries that do not go through an extension, plus the
/// ecosystem/Arch-adjacent names hjkl's grammar set covers.
///
/// Matches are case-sensitive (as in vim: `Dockerfile` ≠ `dockerfile`), with
/// the `Dockerfile.*` / `Containerfile.*` prefix forms handled separately.
pub fn known_filename_language(basename: &str) -> Option<&'static str> {
    // `Dockerfile.dev`, `Containerfile.fedora` — the suffix is the variant
    // name, not a type signal.
    if basename.starts_with("Dockerfile.") || basename.starts_with("Containerfile.") {
        return Some("dockerfile");
    }
    let lang = match basename {
        "CMakeLists.txt" | "CMakeCache.txt" => "cmake",
        "Makefile" | "makefile" | "GNUmakefile" => "make",
        "Dockerfile" | "Containerfile" => "dockerfile",
        "SConstruct" | "SConscript" | "wscript" => "python",
        "meson.build" | "meson_options.txt" => "meson",
        "Jenkinsfile" => "groovy",
        "Justfile" | "justfile" => "just",
        ".gitmodules" => "git_config",
        // vim maps these to `sh`; hjkl ships only the bash grammar.
        ".bashrc" | ".bash_profile" | ".bash_login" | "bashrc" | "bash_profile" | ".profile" => {
            "bash"
        }
        // vim maps these to `zsh`; no zsh grammar, bash is the superset.
        ".zshrc" | ".zshenv" | ".zprofile" | ".zlogin" | "zshrc" => "bash",
        ".vimrc" | "vimrc" | ".gvimrc" | "gvimrc" | ".exrc" => "vim",
        ".tmux.conf" => "tmux",
        ".Xresources" | ".Xdefaults" => "xresources",
        "PKGBUILD" => "bash",
        // The ruby-toolchain family (all ruby scripts by convention).
        "Brewfile" | "Vagrantfile" | "Rakefile" | "Gemfile" | "Guardfile" | "Capfile"
        | "Podfile" | "Thorfile" | "Berksfile" | "Fastfile" | "Appfile" | "Deliverfile" => "ruby",
        "Cargo.lock" => "toml",
        "hyprland.conf" => "hyprlang",
        "nginx.conf" => "nginx",
        "sxhkdrc" => "sxhkdrc",
        "zathurarc" => "zathurarc",
        "Kconfig" => "kconfig",
        _ => return None,
    };
    Some(lang)
}

// ── Shebangs ──────────────────────────────────────────────────────────────────

/// Last path component of an interpreter path (`/usr/bin/env` → `env`).
fn interpreter_basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Map the first line of a file to a language name from its `#!` shebang.
/// Handles `#!/bin/bash`, `#!/usr/bin/env python3`, `env -S` and `-u VAR`
/// forms, and version-suffixed interpreters (`python3.11`, `lua5.4`,
/// `tclsh8.6`, `node20`). Returns `None` when the line is not a shebang or
/// the interpreter is unknown.
pub fn shebang_language(first_line: &str) -> Option<&'static str> {
    // A UTF-8 BOM can precede `#!` (some editors write one); strip it.
    let line = first_line
        .strip_prefix('\u{feff}')
        .unwrap_or(first_line)
        .trim_start();
    let rest = line.strip_prefix("#!")?;
    let mut tokens = rest.split_whitespace();
    let first = tokens.next()?;

    // `#!/usr/bin/env [-flags] INTERP [args]` — find the interpreter, skipping
    // env's own flags. `-u NAME` / `-C DIR` / `-P PATH` consume their
    // argument; everything else starting with `-` is a bare flag (`-i`, `-S`,
    // `-0`, `--split-string=…`).
    if interpreter_basename(first) == "env" {
        let mut skip_next = false;
        for token in tokens {
            if skip_next {
                skip_next = false;
                continue;
            }
            if token.starts_with('-') {
                skip_next = matches!(token, "-u" | "--unset" | "-C" | "-P");
                continue;
            }
            return interpreter_language(interpreter_basename(token));
        }
        return None;
    }

    interpreter_language(interpreter_basename(first))
}

/// Interpreters the shebang table knows, keyed by basename. Version-suffixed
/// names (`python3.11`, `tclsh8.6`, `node20`) are handled by stripping
/// trailing `[0-9.]+` runs until a hit lands (`python3.11` → `python3` →
/// `python`).
fn interpreter_language(name: &str) -> Option<&'static str> {
    let mut name = name;
    loop {
        if let Some(hit) = interpreter_language_exact(name) {
            return Some(hit);
        }
        let stripped = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        if stripped.len() == name.len() {
            return None;
        }
        name = stripped;
    }
}

fn interpreter_language_exact(name: &str) -> Option<&'static str> {
    Some(match name {
        // No separate `sh`/`zsh`/`ksh`/`dash` grammar — bash is the
        // superset, and tree-sitter-bash handles their common syntax.
        "sh" | "bash" | "dash" | "ksh" | "zsh" => "bash",
        "python" | "python2" | "python3" | "pypy" | "pypy3" => "python",
        "perl" => "perl",
        "ruby" => "ruby",
        "node" | "nodejs" | "bun" => "javascript",
        "deno" => "typescript",
        "lua" | "luajit" => "lua",
        "php" => "php",
        "Rscript" | "R" => "r",
        "julia" => "julia",
        "elixir" => "elixir",
        "escript" | "erl" => "erlang",
        "runghc" | "runhaskell" | "ghc" | "stack" => "haskell",
        "ocaml" => "ocaml",
        "tclsh" | "wish" => "tcl",
        "awk" | "gawk" | "nawk" | "mawk" => "awk",
        "gjs" => "gjs",
        "groovy" => "groovy",
        "go" => "go",
        "make" | "gmake" => "make",
        "pwsh" | "powershell" => "powershell",
        "nu" => "nu",
        "vim" => "vim",
        "sbcl" | "clisp" => "commonlisp",
        "guile" => "scheme",
        "racket" => "racket",
        "clojure" => "clojure",
        "crystal" => "crystal",
        "swift" => "swift",
        "scala" => "scala",
        "dart" => "dart",
        "nix-shell" => "nix",
        "fish" => "fish",
        _ => return None,
    })
}

// ── Modeline `ft=` ────────────────────────────────────────────────────────────

/// Cap on how much of a scanned line the modeline parser reads, in chars.
/// A modeline is a short comment (`# vim: ft=rust`); nothing realistic sits
/// past this, and the cap bounds the worst case (a single giant line) no
/// matter how big the file is. A deliberate hjkl bound — neovim scans the
/// whole line, but only the first/last `'modelines'` lines.
const MODELINE_LINE_CAP: usize = 500;

/// First [`MODELINE_LINE_CAP`] chars of `line`, cut at a char boundary.
fn cap_modeline_line(line: &str) -> &str {
    match line.char_indices().nth(MODELINE_LINE_CAP) {
        Some((byte_idx, _)) => &line[..byte_idx],
        None => line,
    }
}

/// Extract the `ft=` / `filetype=` value from the first/last `depth` lines of
/// `content`, mirroring the marker, `set` keyword and terminating-colon rules
/// of the full modeline parser (`hjkl-app`'s `modeline` module) but reading
/// only the filetype token.
///
/// Scan order and per-line bound follow the perf contract: only the first
/// and last `depth` lines are examined — never the middle — and each line is
/// read for at most [`MODELINE_LINE_CAP`] chars. `lines().rev()` is
/// double-ended, so the bottom pass starts from the end via a backward
/// newline search: a huge file costs O(depth) lines, not O(lines), and no
/// line table is allocated.
///
/// Later lines win, matching vim/nvim: every scanned modeline applies in
/// line order, so the `ft=` on the highest line number overrides earlier
/// ones (verified against neovim 0.12.4 — a top `ft=bash` with a bottom
/// `ft=python` yields `python`, including when the two scans overlap on a
/// small file). Within a line, the last `ft=` token wins (vim applies
/// options left to right).
fn modeline_filetype(content: &str, depth: usize) -> Option<String> {
    // Bottom `depth` lines first, highest line number first: the first hit
    // is on the highest line carrying an `ft=`, which is the winner — every
    // bottom line outranks every top-only line (the ranges only overlap
    // where the bottom pass has already looked).
    for line in content.lines().rev().take(depth) {
        if let Some(ft) = modeline_filetype_on_line(line) {
            return Some(ft);
        }
    }
    // Top `depth` lines, reached only when no bottom line had an `ft=`.
    // Scan forward and keep the last hit so the highest top line wins.
    // Overlapping lines were already scanned by the bottom pass; they are
    // re-checked here only when they carried no `ft=` (they would have
    // returned above), so the redundancy is bounded by `depth` and harmless.
    let mut found = None;
    for line in content.lines().take(depth) {
        if let Some(ft) = modeline_filetype_on_line(line) {
            found = Some(ft);
        }
    }
    found
}

/// `ft=` extraction for a single line; `None` when the line carries no
/// modeline or no filetype token. Reads at most [`MODELINE_LINE_CAP`] chars.
fn modeline_filetype_on_line(line: &str) -> Option<String> {
    let line = cap_modeline_line(line);
    let (marker_start, rest) = find_modeline_marker(line)?;
    // The character before the marker must be line start or non-alphanumeric,
    // so `xvim:` is rejected but `// vim:` and `#vim:` are accepted.
    if marker_start > 0 {
        let before = line[..marker_start].chars().next_back()?;
        if before.is_alphanumeric() {
            return None;
        }
    }
    let rest = rest.trim_start();
    let body = rest
        .strip_prefix("set ")
        .or_else(|| rest.strip_prefix("set\t"))
        .unwrap_or(rest);

    let mut found: Option<String> = None;
    for token in body.split_whitespace() {
        let (token, terminates) = match token.strip_suffix(':') {
            Some(stripped) => (stripped, true),
            None => (token, false),
        };
        if let Some((key, value)) = token.split_once('=')
            && matches!(key, "ft" | "filetype")
            && !value.is_empty()
        {
            found = Some(value.to_string());
        }
        if terminates {
            break;
        }
    }
    found
}

/// Find the earliest `vim:` / `ex:` / `vi:` marker in `line`, returning the
/// byte offset of the marker and the text after the colon. Ties (a `vim:`
/// line also contains `vi:`) resolve to the first-listed marker, matching
/// vim's first-marker parse.
fn find_modeline_marker(line: &str) -> Option<(usize, &str)> {
    ["vim:", "ex:", "vi:"]
        .iter()
        .filter_map(|marker| {
            line.find(marker)
                .map(|pos| (pos, &line[pos + marker.len()..]))
        })
        .min_by_key(|(pos, _)| *pos)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir() -> LanguageDirectory {
        LanguageDirectory::new().unwrap()
    }

    fn detect(path: &str, content: &str, opts: &DetectOptions) -> Option<String> {
        dir().detect(&PathBuf::from(path), content, opts)
    }

    // ── Extension ────────────────────────────────────────────────────────────

    #[test]
    fn extension_wins_over_everything() {
        // A `.py` extension beats a conflicting shebang and modeline.
        let content = "#!/bin/bash\n# vim: ft=ruby\n";
        assert_eq!(
            detect("script.py", content, &DetectOptions::default()),
            Some("python".to_string())
        );
    }

    #[test]
    fn known_extension_with_no_other_signal() {
        assert_eq!(
            detect("main.rs", "", &DetectOptions::default()),
            Some("rust".into())
        );
    }

    #[test]
    fn unknown_extension_is_not_a_signal() {
        // `.txt` has no grammar, so the filename/shebang/modeline steps run.
        let content = "#!/usr/bin/env python3\n";
        assert_eq!(
            detect("notes.txt", content, &DetectOptions::default()),
            Some("python".into())
        );
    }

    // ── Known basenames ──────────────────────────────────────────────────────

    #[test]
    fn known_filenames_resolve() {
        for (name, want) in [
            ("Makefile", "make"),
            ("GNUmakefile", "make"),
            ("CMakeLists.txt", "cmake"),
            ("Dockerfile", "dockerfile"),
            ("Dockerfile.dev", "dockerfile"),
            ("Containerfile", "dockerfile"),
            ("Jenkinsfile", "groovy"),
            ("Justfile", "just"),
            ("PKGBUILD", "bash"),
            ("Cargo.lock", "toml"),
            ("Gemfile", "ruby"),
            ("meson.build", "meson"),
            ("Kconfig", "kconfig"),
            (".gitmodules", "git_config"),
            ("hyprland.conf", "hyprlang"),
        ] {
            assert_eq!(
                detect(name, "", &DetectOptions::default()),
                Some(want.to_string()),
                "{name} should detect as {want}"
            );
        }
    }

    #[test]
    fn dockerfile_prefix_forms_resolve() {
        assert_eq!(
            detect("Dockerfile.prod", "", &DetectOptions::default()),
            Some("dockerfile".into())
        );
        assert_eq!(
            detect("Containerfile.fedora", "", &DetectOptions::default()),
            Some("dockerfile".into())
        );
    }

    #[test]
    fn filename_matching_is_case_sensitive() {
        assert_eq!(
            detect("makefile", "", &DetectOptions::default()),
            Some("make".into())
        );
        assert_eq!(detect("dockerfile", "", &DetectOptions::default()), None);
    }

    #[test]
    fn known_basename_beats_catchall_extension() {
        // The systemd grammar claims the `build` extension; a specific
        // basename must still win so meson.build is not highlighted as
        // systemd.
        assert_eq!(
            detect("meson.build", "", &DetectOptions::default()),
            Some("meson".into())
        );
        assert_eq!(
            detect("meson_options.txt", "", &DetectOptions::default()),
            Some("meson".into())
        );
    }

    #[test]
    fn unknown_basename_falls_through_to_content() {
        assert_eq!(
            detect("script", "#!/bin/bash\n", &DetectOptions::default()),
            Some("bash".into())
        );
    }

    // ── Shebangs ─────────────────────────────────────────────────────────────

    #[test]
    fn shebang_direct_path() {
        assert_eq!(shebang_language("#!/bin/bash"), Some("bash"));
        assert_eq!(shebang_language("#!/usr/bin/python3"), Some("python"));
        assert_eq!(shebang_language("#!/bin/sh"), Some("bash"));
    }

    #[test]
    fn shebang_env_form() {
        assert_eq!(shebang_language("#!/usr/bin/env bash"), Some("bash"));
        assert_eq!(shebang_language("#!/usr/bin/env node"), Some("javascript"));
        assert_eq!(
            shebang_language("#!/usr/bin/env -S bash -euo pipefail"),
            Some("bash")
        );
        assert_eq!(
            shebang_language("#!/usr/bin/env -u FOO -S python3 -u"),
            Some("python")
        );
    }

    #[test]
    fn shebang_version_suffixes() {
        assert_eq!(
            shebang_language("#!/usr/bin/env python3.11"),
            Some("python")
        );
        assert_eq!(shebang_language("#!/usr/bin/lua5.4"), Some("lua"));
        assert_eq!(shebang_language("#!/usr/bin/tclsh8.6"), Some("tcl"));
        assert_eq!(
            shebang_language("#!/usr/bin/env node20"),
            Some("javascript")
        );
        assert_eq!(shebang_language("#!/usr/bin/perl5"), Some("perl"));
    }

    #[test]
    fn shebang_unknown_interpreter_is_none() {
        assert_eq!(shebang_language("#!/usr/bin/env definitely-not-real"), None);
        assert_eq!(shebang_language("#!nonsense"), None);
    }

    #[test]
    fn non_shebang_line_is_none() {
        assert_eq!(shebang_language("# not a shebang"), None);
        assert_eq!(shebang_language("hello"), None);
        assert_eq!(shebang_language(""), None);
    }

    #[test]
    fn shebang_with_bom() {
        assert_eq!(shebang_language("\u{feff}#!/bin/bash"), Some("bash"));
    }

    #[test]
    fn modeline_beats_shebang() {
        // vim parity: an explicit modeline `ft=` outranks the shebang
        // heuristic, even when the shebang contradicts it.
        let content = "#!/usr/bin/env bash\n# vim: ft=python\n";
        assert_eq!(
            detect("script", content, &DetectOptions::default()),
            Some("python".into())
        );
    }

    // ── Modeline `ft=` ───────────────────────────────────────────────────────

    #[test]
    fn modeline_ft_forms() {
        for line in [
            "# vim: ft=bash",
            "# vim: set ft=bash:",
            "# vi: filetype=python",
            "# ex: set filetype=lua:",
            "/* vim: set ft=rust: */",
            "#vim: ft=go",
        ] {
            let content = format!("{line}\n");
            assert_eq!(
                detect("script", &content, &DetectOptions::default()),
                Some(
                    match line {
                        l if l.contains("bash") => "bash",
                        l if l.contains("python") => "python",
                        l if l.contains("lua") => "lua",
                        l if l.contains("rust") => "rust",
                        _ => "go",
                    }
                    .to_string()
                ),
                "modeline {line:?} should detect"
            );
        }
    }

    #[test]
    fn modeline_last_ft_wins() {
        let content = "# vim: ft=bash ft=python\n";
        assert_eq!(
            detect("script", content, &DetectOptions::default()),
            Some("python".into())
        );
    }

    #[test]
    fn modeline_in_last_lines_is_scanned() {
        // 10-line file, modeline only on the last line, depth 3.
        let mut lines: Vec<String> = (0..9).map(|i| format!("line {i}")).collect();
        lines.push("# vim: ft=bash:".to_string());
        let content = lines.join("\n");
        assert_eq!(
            detect("script", &content, &DetectOptions::default()),
            Some("bash".into())
        );
    }

    #[test]
    fn modeline_outside_scan_depth_is_ignored() {
        // Modeline at line index 5 of a 12-line file with depth 5 is not
        // covered (top 0..5, bottom 7..12).
        let mut lines: Vec<String> = (0..12).map(|i| format!("line {i}")).collect();
        lines[5] = "# vim: ft=python:".to_string();
        let content = lines.join("\n");
        assert_eq!(detect("script", &content, &DetectOptions::default()), None);
    }

    #[test]
    fn modeline_disabled_by_option() {
        let content = "# vim: ft=bash\n";
        let opts = DetectOptions {
            modeline: false,
            ..DetectOptions::default()
        };
        assert_eq!(detect("script", content, &opts), None);
    }

    #[test]
    fn modeline_marker_must_be_word_boundary() {
        assert_eq!(
            detect("script", "xvim: ft=bash\n", &DetectOptions::default()),
            None
        );
    }

    #[test]
    fn modeline_non_ft_options_do_not_detect() {
        assert_eq!(
            detect(
                "script",
                "# vim: ts=2 sw=2 et:\n",
                &DetectOptions::default()
            ),
            None
        );
    }

    #[test]
    fn modeline_bottom_line_wins() {
        // Later lines win (verified against neovim 0.12.4): a bottom
        // `ft=` overrides a top `ft=`.
        let mut lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        lines[0] = "# vim: ft=bash".to_string();
        lines[99] = "# vim: ft=python".to_string();
        let content = lines.join("\n");
        assert_eq!(
            detect("script", &content, &DetectOptions::default()),
            Some("python".into())
        );
    }

    #[test]
    fn modeline_later_line_wins_with_overlap() {
        // 8-line file, depth 5: top scan covers 0-4, bottom covers 3-7, so
        // lines 3 and 4 are in both ranges. Line 4 is later and must win
        // (verified against neovim 0.12.4).
        let mut lines: Vec<String> = (0..8).map(|i| format!("line {i}")).collect();
        lines[3] = "# vim: ft=bash".to_string();
        lines[4] = "# vim: ft=python".to_string();
        let content = lines.join("\n");
        assert_eq!(
            detect("script", &content, &DetectOptions::default()),
            Some("python".into())
        );
    }

    #[test]
    fn modeline_beyond_line_cap_is_not_detected() {
        // Per-line scan is capped at MODELINE_LINE_CAP chars; a modeline
        // sitting past the cap is invisible (deliberate hjkl bound).
        let mut line = "x".repeat(600);
        line.push_str(" # vim: ft=bash");
        let content = format!("{line}\n");
        assert_eq!(detect("script", &content, &DetectOptions::default()), None);
    }

    #[test]
    fn modeline_within_line_cap_is_detected() {
        let mut line = "x".repeat(400);
        line.push_str(" # vim: ft=bash");
        let content = format!("{line}\n");
        assert_eq!(
            detect("script", &content, &DetectOptions::default()),
            Some("bash".into())
        );
    }

    #[test]
    fn modeline_cap_respects_char_boundaries() {
        // Multibyte chars before the modeline must not panic the slice — the
        // cap is taken at a char boundary.
        let mut line = "你".repeat(300); // 900 bytes, 300 chars
        line.push_str(" # vim: ft=bash");
        let content = format!("{line}\n");
        assert_eq!(
            detect("script", &content, &DetectOptions::default()),
            Some("bash".into())
        );
    }

    // ── Precedence and gaps ──────────────────────────────────────────────────

    #[test]
    fn empty_content_and_unknown_name_is_none() {
        assert_eq!(detect("script", "", &DetectOptions::default()), None);
        assert_eq!(
            detect("script", "just some text\n", &DetectOptions::default()),
            None
        );
    }

    #[test]
    fn new_file_with_known_name_still_detects() {
        // A brand-new file has no content yet; the basename still resolves.
        assert_eq!(
            detect("Makefile", "", &DetectOptions::default()),
            Some("make".into())
        );
    }
}
