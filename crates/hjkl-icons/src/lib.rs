//! Filetype / directory → icon mapping for the hjkl editor stack.
//!
//! Pure mapping: given a file extension or directory name plus an [`IconMode`],
//! returns a single-character icon. Three modes:
//!
//! - [`IconMode::Nerd`] — Nerd-Font glyphs, keyed per filetype (needs a patched
//!   font).
//! - [`IconMode::Unicode`] — geometric Unicode fallback (`▸ ▾ ·`), renders in
//!   virtually every monospace font.
//! - [`IconMode::Ascii`] — strict ASCII fallback (`> v`), works everywhere.
//!
//! There is no portable way to detect whether the terminal's font has Nerd
//! glyphs (terminals don't expose their font; a missing glyph and a real one
//! both usually occupy one cell). So this crate is a pure mapping and takes the
//! already-resolved mode — the host owns the `nerd|unicode|ascii|auto` setting
//! and any best-effort probe.

#![forbid(unsafe_code)]

use std::path::Path;

/// Which icon set to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconMode {
    /// Nerd-Font glyphs (per-filetype). Requires a patched font.
    #[default]
    Nerd,
    /// Geometric Unicode fallback (`▸ ▾ ·`) — works in nearly all monospace fonts.
    Unicode,
    /// Strict ASCII fallback (`> v`) — works literally everywhere.
    Ascii,
}

impl IconMode {
    /// Parse an explicit mode from a config string (`"nerd"`, `"unicode"`,
    /// `"ascii"`; case-insensitive). Returns `None` for anything else —
    /// including `"auto"`, which is a host concern (resolve it by probing the
    /// terminal, then pass the concrete mode here).
    pub fn from_config(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nerd" => Some(Self::Nerd),
            "unicode" => Some(Self::Unicode),
            "ascii" => Some(Self::Ascii),
            _ => None,
        }
    }
}

/// Icon for a directory, given its `name` (for special folders like `.git`) and
/// whether it is expanded. In non-Nerd modes the icon doubles as the expand
/// indicator (`▸`/`▾`, `>`/`v`).
pub fn dir_icon(name: Option<&str>, expanded: bool, mode: IconMode) -> char {
    match mode {
        IconMode::Nerd => nerd_dir_icon(name, expanded),
        IconMode::Unicode => {
            if expanded {
                '\u{25be}' // ▾
            } else {
                '\u{25b8}' // ▸
            }
        }
        IconMode::Ascii => {
            if expanded {
                'v'
            } else {
                '>'
            }
        }
    }
}

/// Icon for a file, given its extension (any case; `None` / unknown → generic).
/// Non-Nerd modes return a single generic file mark (no per-type glyphs).
pub fn file_icon(ext: Option<&str>, mode: IconMode) -> char {
    match mode {
        IconMode::Nerd => nerd_file_icon(ext),
        IconMode::Unicode => '\u{b7}', // ·
        IconMode::Ascii => ' ',
    }
}

/// Convenience: directory icon for a filesystem path.
pub fn dir_icon_for_path(path: &Path, expanded: bool, mode: IconMode) -> char {
    dir_icon(path.file_name().and_then(|n| n.to_str()), expanded, mode)
}

/// Convenience: file icon for a filesystem path (keyed by extension).
pub fn file_icon_for_path(path: &Path, mode: IconMode) -> char {
    let ext = path.extension().and_then(|e| e.to_str());
    file_icon(ext, mode)
}

/// Devicon-style RGB color for a file, keyed by extension (case-insensitive).
///
/// Returns `None` for unknown extensions — callers should fall back to the
/// active theme's default text color. Colors mirror the nvim-web-devicons
/// palette used by neo-tree.
pub fn file_color_for_path(path: &Path) -> Option<(u8, u8, u8)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some((0xde, 0xa5, 0x84)),              // rust orange
        "md" | "markdown" => Some((0x51, 0x9a, 0xba)), // markdown blue-grey
        "toml" | "ini" | "conf" | "cfg" => Some((0x9c, 0x4d, 0x21)), // toml brown/rust
        "lock" => Some((0x80, 0x80, 0x80)),            // grey
        "json" => Some((0xcb, 0xcb, 0x41)),            // json yellow
        "yaml" | "yml" => Some((0xcb, 0xcb, 0x41)),    // yaml yellow
        "html" | "htm" => Some((0xe4, 0x4d, 0x26)),    // html orange-red
        "css" | "scss" => Some((0x56, 0x4f, 0xd8)),    // css purple
        "js" | "mjs" | "cjs" => Some((0xcb, 0xcb, 0x41)), // js yellow
        "ts" | "tsx" => Some((0x00, 0x7a, 0xcc)),      // ts blue
        "py" => Some((0x45, 0x84, 0xb6)),              // python blue
        "go" => Some((0x00, 0xad, 0xd8)),              // go cyan
        "sh" | "bash" | "zsh" | "fish" => Some((0x4e, 0xaa, 0x25)), // shell green
        "txt" | "text" => Some((0x80, 0x80, 0x80)),    // plain grey
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => Some((0xa0, 0x74, 0xc4)), // image purple
        _ => None,
    }
}

/// Devicon-style RGB color for a directory.
///
/// Returns a folder-blue consistent with VS Code / neo-tree. The `_name`
/// parameter is reserved for future special-folder coloring (e.g. `.git` in
/// a distinct hue); pass the directory's file-name string or `None`.
pub fn dir_color(_name: Option<&str>) -> Option<(u8, u8, u8)> {
    // Folder blue — matches VS Code's default folder icon color.
    Some((0x56, 0x9c, 0xd6))
}

fn nerd_dir_icon(name: Option<&str>, expanded: bool) -> char {
    if let Some(name) = name {
        match name {
            ".git" => return '\u{e702}',         //
            ".github" => return '\u{e709}',      //
            ".config" => return '\u{f013}',      //
            "node_modules" => return '\u{e718}', //
            _ => {}
        }
    }
    if expanded {
        '\u{f0770}' // 󰝰 open folder
    } else {
        '\u{f024b}' // 󰉋 closed folder
    }
}

fn nerd_file_icon(ext: Option<&str>) -> char {
    let ext = ext.unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => '\u{f1617}',                                                   // 󱘗
        "md" | "markdown" => '\u{f0354}',                                      // 󰍔
        "toml" | "ini" | "conf" | "cfg" => '\u{f013}',                         //
        "lock" => '\u{f023}',                                                  //
        "json" => '\u{e60b}',                                                  //
        "yaml" | "yml" => '\u{f0626}',                                         // 󰘦
        "html" | "htm" => '\u{f031d}',                                         // 󰌝
        "css" | "scss" => '\u{e749}',                                          //
        "js" | "mjs" | "cjs" => '\u{e74e}',                                    //
        "ts" | "tsx" => '\u{e628}',                                            //
        "py" => '\u{e606}',                                                    //
        "go" => '\u{e627}',                                                    //
        "sh" | "bash" | "zsh" | "fish" => '\u{f489}',                          //
        "txt" | "text" => '\u{f0219}',                                         // 󰈙
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => '\u{f1c5}', //
        _ => '\u{f0214}',                                                      // 󰈔 generic document
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn from_config_parses_known_modes() {
        assert_eq!(IconMode::from_config("nerd"), Some(IconMode::Nerd));
        assert_eq!(IconMode::from_config("UNICODE"), Some(IconMode::Unicode));
        assert_eq!(IconMode::from_config(" ascii "), Some(IconMode::Ascii));
        assert_eq!(IconMode::from_config("auto"), None);
        assert_eq!(IconMode::from_config("bogus"), None);
    }

    #[test]
    fn nerd_file_icons_keyed_by_ext_case_insensitive() {
        assert_eq!(file_icon(Some("rs"), IconMode::Nerd), '\u{f1617}');
        assert_eq!(file_icon(Some("RS"), IconMode::Nerd), '\u{f1617}');
        assert_eq!(file_icon(Some("zzz"), IconMode::Nerd), '\u{f0214}');
        assert_eq!(file_icon(None, IconMode::Nerd), '\u{f0214}');
    }

    #[test]
    fn unicode_and_ascii_fallbacks() {
        assert_eq!(file_icon(Some("rs"), IconMode::Unicode), '\u{b7}');
        assert_eq!(file_icon(Some("rs"), IconMode::Ascii), ' ');
        assert_eq!(dir_icon(None, true, IconMode::Unicode), '\u{25be}');
        assert_eq!(dir_icon(None, false, IconMode::Unicode), '\u{25b8}');
        assert_eq!(dir_icon(None, true, IconMode::Ascii), 'v');
        assert_eq!(dir_icon(None, false, IconMode::Ascii), '>');
    }

    #[test]
    fn nerd_special_folders() {
        assert_eq!(dir_icon(Some(".git"), false, IconMode::Nerd), '\u{e702}');
        // Special-folder icons are Nerd-only; fallbacks use the plain caret.
        assert_eq!(dir_icon(Some(".git"), false, IconMode::Unicode), '\u{25b8}');
    }

    #[test]
    fn path_convenience() {
        assert_eq!(
            file_icon_for_path(Path::new("src/main.rs"), IconMode::Nerd),
            '\u{f1617}'
        );
        assert_eq!(
            dir_icon_for_path(Path::new("/proj/.git"), false, IconMode::Nerd),
            '\u{e702}'
        );
    }

    #[test]
    fn file_color_known_extensions_return_some() {
        // Rust — devicon orange
        assert_eq!(
            file_color_for_path(Path::new("src/main.rs")),
            Some((0xde, 0xa5, 0x84))
        );
        // Python — devicon blue
        assert_eq!(
            file_color_for_path(Path::new("script.py")),
            Some((0x45, 0x84, 0xb6))
        );
        // JSON — yellow
        assert_eq!(
            file_color_for_path(Path::new("package.json")),
            Some((0xcb, 0xcb, 0x41))
        );
    }

    #[test]
    fn file_color_unknown_extension_returns_none() {
        assert_eq!(file_color_for_path(Path::new("binary.exe")), None);
        assert_eq!(file_color_for_path(Path::new("no_extension")), None);
        assert_eq!(file_color_for_path(Path::new("")), None);
    }

    #[test]
    fn file_color_case_insensitive() {
        // Extension matching is case-insensitive.
        assert_eq!(
            file_color_for_path(Path::new("main.RS")),
            file_color_for_path(Path::new("main.rs"))
        );
        assert_eq!(
            file_color_for_path(Path::new("app.PY")),
            file_color_for_path(Path::new("app.py"))
        );
    }

    #[test]
    fn dir_color_returns_folder_blue() {
        let color = dir_color(None);
        assert_eq!(color, Some((0x56, 0x9c, 0xd6)));
        // Named dirs also return the folder blue (no special casing yet).
        assert_eq!(dir_color(Some("src")), Some((0x56, 0x9c, 0xd6)));
    }
}
