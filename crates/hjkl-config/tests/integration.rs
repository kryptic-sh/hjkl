use std::io::Write as _;

use hjkl_config::{
    AppConfig, ConfigError, ConfigSource, Validate, load_from, load_layered_from, write_default,
};
use serde::{Deserialize, Serialize};
use tempfile::{NamedTempFile, tempdir};

#[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
struct Fixture {
    #[serde(default)]
    name: String,
    #[serde(default)]
    count: u32,
}

impl AppConfig for Fixture {
    const APPLICATION: &'static str = "hjkl-config-test-fixture";
}

#[derive(Debug, thiserror::Error)]
#[error("count must be > 0")]
struct ZeroCount;

impl Validate for Fixture {
    type Error = ZeroCount;
    fn validate(&self) -> Result<(), Self::Error> {
        if self.count == 0 {
            Err(ZeroCount)
        } else {
            Ok(())
        }
    }
}

#[test]
fn load_from_explicit_path_ok() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "name = \"alice\"\ncount = 7").unwrap();
    let cfg: Fixture = load_from(f.path()).unwrap();
    assert_eq!(
        cfg,
        Fixture {
            name: "alice".into(),
            count: 7,
        }
    );
}

#[test]
fn load_from_missing_file_is_io_error() {
    let err = load_from::<Fixture>(std::path::Path::new("/no/such/file.toml")).unwrap_err();
    assert!(matches!(err, ConfigError::Io { .. }));
}

#[test]
fn load_from_malformed_toml_reports_span() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "name = \"alice\"\ncount = not_a_number").unwrap();
    let err = load_from::<Fixture>(f.path()).unwrap_err();
    let ConfigError::Parse {
        line, col, snippet, ..
    } = err
    else {
        panic!("expected Parse, got {err:?}");
    };
    assert_eq!(line, 2, "error should point at line 2");
    assert!(col > 0);
    assert!(snippet.contains("count"));
}

#[test]
fn validate_hook_reports_consumer_errors() {
    let cfg = Fixture {
        name: "bob".into(),
        count: 0,
    };
    assert!(cfg.validate().is_err());
    let cfg = Fixture {
        name: "bob".into(),
        count: 1,
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn write_default_creates_parent_dirs() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("nested/sub/config.toml");
    let cfg = Fixture {
        name: "carol".into(),
        count: 3,
    };
    write_default(&path, &cfg).unwrap();
    assert!(path.exists());
    let roundtrip: Fixture = load_from(&path).unwrap();
    assert_eq!(roundtrip, cfg);
}

#[test]
fn config_source_marks_file_vs_defaults() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "name = \"x\"\ncount = 1").unwrap();
    let cfg: Fixture = load_from(f.path()).unwrap();
    assert_eq!(cfg.name, "x");

    let from = ConfigSource::File(f.path().to_path_buf());
    assert_ne!(from, ConfigSource::Defaults);
}

// ── Layered loading (bundled defaults + user overrides) ───────────────────

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Layered {
    editor: LayeredEditor,
    theme: LayeredTheme,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct LayeredEditor {
    leader: char,
    tab_width: u8,
    expandtab: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct LayeredTheme {
    name: String,
}

const LAYERED_DEFAULTS: &str = r#"
[editor]
leader = " "
tab_width = 4
expandtab = true

[theme]
name = "dark"
"#;

impl Default for Layered {
    fn default() -> Self {
        toml::from_str(LAYERED_DEFAULTS).unwrap()
    }
}

impl AppConfig for Layered {
    const APPLICATION: &'static str = "hjkl-config-layered-fixture";
}

#[test]
fn layered_partial_override_keeps_unspecified_defaults() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[editor]\nleader = \"\\\\\"").unwrap();
    let cfg: Layered = load_layered_from(LAYERED_DEFAULTS, f.path()).unwrap();
    assert_eq!(cfg.editor.leader, '\\');
    assert_eq!(cfg.editor.tab_width, 4, "tab_width should keep default");
    assert!(cfg.editor.expandtab, "expandtab should keep default");
    assert_eq!(cfg.theme.name, "dark", "theme.name should keep default");
}

#[test]
fn layered_full_override_takes_precedence() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(
        f,
        r#"
[editor]
leader = ","
tab_width = 2
expandtab = false

[theme]
name = "light"
"#
    )
    .unwrap();
    let cfg: Layered = load_layered_from(LAYERED_DEFAULTS, f.path()).unwrap();
    assert_eq!(cfg.editor.leader, ',');
    assert_eq!(cfg.editor.tab_width, 2);
    assert!(!cfg.editor.expandtab);
    assert_eq!(cfg.theme.name, "light");
}

#[test]
fn layered_unknown_user_key_is_invalid() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[editor]\nbogus = 1").unwrap();
    let err = load_layered_from::<Layered>(LAYERED_DEFAULTS, f.path()).unwrap_err();
    assert!(
        matches!(err, ConfigError::Invalid { .. }),
        "expected Invalid, got {err:?}"
    );
}

#[test]
fn layered_malformed_user_toml_reports_span() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[editor]\nleader = not_quoted").unwrap();
    let err = load_layered_from::<Layered>(LAYERED_DEFAULTS, f.path()).unwrap_err();
    let ConfigError::Parse { line, .. } = err else {
        panic!("expected Parse, got {err:?}");
    };
    assert_eq!(line, 2);
}

#[test]
fn layered_invalid_bundled_defaults_is_invalid() {
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[editor]\nleader = \" \"").unwrap();
    let err = load_layered_from::<Layered>("nonsense {{ broken", f.path()).unwrap_err();
    let ConfigError::Invalid { path, .. } = err else {
        panic!("expected Invalid, got {err:?}");
    };
    assert!(
        path.to_string_lossy().contains("bundled defaults"),
        "path should mark bundled defaults as the source: {path:?}"
    );
}

#[test]
fn layered_nested_tables_merge_recursively() {
    // Verify that a partial inner-table override doesn't wipe sibling keys.
    let mut f = NamedTempFile::new().unwrap();
    writeln!(f, "[editor]\ntab_width = 8").unwrap();
    let cfg: Layered = load_layered_from(LAYERED_DEFAULTS, f.path()).unwrap();
    assert_eq!(cfg.editor.tab_width, 8);
    assert_eq!(cfg.editor.leader, ' ', "sibling key must survive merge");
    assert!(cfg.editor.expandtab, "sibling key must survive merge");
}
