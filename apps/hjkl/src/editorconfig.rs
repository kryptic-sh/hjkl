//! EditorConfig integration — load project-local indent rules and
//! map them onto an `hjkl_engine::types::Options`.

use std::path::Path;

use ec4rs::property::{IndentSize, IndentStyle, MaxLineLen, TabWidth};
use hjkl_engine::types::Options;

/// Override fields of `opts` based on the editorconfig stack discovered
/// upward from `path`. Falls back silently on parse / IO errors — the
/// caller already has sensible defaults.
pub fn overlay_for_path(opts: &mut Options, path: &Path) {
    let mut props = match ec4rs::properties_of(path) {
        Ok(p) => p,
        Err(_) => return,
    };
    props.use_fallbacks();

    // indent_style → expandtab
    if let Ok(style) = props.get::<IndentStyle>() {
        opts.expandtab = match style {
            IndentStyle::Spaces => true,
            IndentStyle::Tabs => false,
        };
    }

    // tab_width → tabstop (read first so indent_size fallback below can see it)
    let explicit_tab_width: Option<usize> = props.get::<TabWidth>().ok().map(|tw| match tw {
        TabWidth::Value(n) => n,
    });
    if let Some(tw) = explicit_tab_width {
        opts.tabstop = tw as u32;
    }

    // indent_size → shiftwidth + softtabstop
    // indent_size = tab  → UseTabWidth: shiftwidth = tabstop, softtabstop = 0
    // indent_size = N    → shiftwidth = N, softtabstop = N
    //                      if tab_width was absent, also set tabstop = N
    match props.get::<IndentSize>() {
        Ok(IndentSize::UseTabWidth) => {
            opts.shiftwidth = opts.tabstop;
            opts.softtabstop = 0;
        }
        Ok(IndentSize::Value(n)) => {
            let n = n as u32;
            opts.shiftwidth = n;
            opts.softtabstop = n;
            // Vim heuristic: if tab_width was not explicitly set, mirror indent_size
            if explicit_tab_width.is_none() {
                opts.tabstop = n;
            }
        }
        Err(_) => {}
    }

    // max_line_length → textwidth
    if let Ok(MaxLineLen::Value(n)) = props.get::<MaxLineLen>() {
        opts.textwidth = n as u32;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Write an `.editorconfig` into `dir` and return the path to a dummy
    /// target file inside that directory (the file need not exist on disk;
    /// ec4rs only uses it for glob matching and directory walking).
    fn write_editorconfig(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        fs::write(dir.join(".editorconfig"), content).unwrap();
        dir.join("dummy.rs")
    }

    #[test]
    fn overlays_indent_style_tab() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_editorconfig(dir.path(), "[*]\nindent_style = tab\n");
        let mut opts = Options::default();
        overlay_for_path(&mut opts, &target);
        assert!(
            !opts.expandtab,
            "indent_style=tab should set expandtab=false"
        );
    }

    #[test]
    fn overlays_indent_size_two() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_editorconfig(dir.path(), "[*]\nindent_size = 2\n");
        let mut opts = Options::default();
        overlay_for_path(&mut opts, &target);
        assert_eq!(opts.shiftwidth, 2, "shiftwidth should be 2");
        assert_eq!(opts.softtabstop, 2, "softtabstop should be 2");
        assert_eq!(
            opts.tabstop, 2,
            "tabstop should be inferred from indent_size when tab_width absent"
        );
    }

    #[test]
    fn tab_width_separate_from_indent_size() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_editorconfig(dir.path(), "[*]\ntab_width = 8\nindent_size = 4\n");
        let mut opts = Options::default();
        overlay_for_path(&mut opts, &target);
        assert_eq!(opts.tabstop, 8, "tabstop should come from tab_width");
        assert_eq!(
            opts.shiftwidth, 4,
            "shiftwidth should come from indent_size"
        );
        assert_eq!(
            opts.softtabstop, 4,
            "softtabstop should come from indent_size"
        );
    }

    #[test]
    fn max_line_length_sets_textwidth() {
        let dir = tempfile::tempdir().unwrap();
        let target = write_editorconfig(dir.path(), "[*]\nmax_line_length = 100\n");
        let mut opts = Options::default();
        overlay_for_path(&mut opts, &target);
        assert_eq!(opts.textwidth, 100, "textwidth should be 100");
    }

    #[test]
    fn glob_matches_only_rs() {
        let dir = tempfile::tempdir().unwrap();
        // Only .rs files get indent_size = 2
        fs::write(
            dir.path().join(".editorconfig"),
            "[*.rs]\nindent_size = 2\n",
        )
        .unwrap();

        // .rs file should be overridden
        let rs_file = dir.path().join("src.rs");
        let mut opts_rs = Options::default();
        overlay_for_path(&mut opts_rs, &rs_file);
        assert_eq!(opts_rs.shiftwidth, 2, ".rs file should get shiftwidth=2");

        // .go file should keep defaults
        let go_file = dir.path().join("main.go");
        let mut opts_go = Options::default();
        overlay_for_path(&mut opts_go, &go_file);
        assert_eq!(
            opts_go.shiftwidth,
            Options::default().shiftwidth,
            ".go file should keep default shiftwidth"
        );
    }
}
