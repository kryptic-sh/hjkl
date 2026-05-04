//! Headless neovim diff harness for vim-compatibility regression testing.
//!
//! Each [`OracleCase`] encodes an initial buffer state, a sequence of vim
//! keystrokes (in vim macro notation), and one or more expected outcomes
//! (buffer content, cursor position, mode, register contents). The oracle
//! drives both `hjkl-engine` and a headless neovim process with identical
//! inputs and diffs the results, surfacing regressions before they ship.
//!
//! Cases are grouped into [`Corpus`] files loaded from TOML via
//! [`load_corpus`]. See [issue #23](https://github.com/kryptic-sh/hjkl/issues/23)
//! for the full design.

use std::path::Path;

pub mod diff;
pub mod hjkl_driver;
pub mod nvim_driver;
mod test_host;

pub use diff::{CaseResult, CaseStatus, run_oracle};
pub use hjkl_driver::HjklOutcome;
pub use nvim_driver::{NvimOutcome, nvim_available};

/// A single vim-compatibility test case.
///
/// All string keys use vim macro notation (`<Esc>`, `<C-r>`, `dd`, ...).
/// Cursor positions are 0-based `(row, col)` with col measured in bytes
/// within the line.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OracleCase {
    /// Human-readable case identifier, e.g. `"motion_w_basic"`.
    pub name: String,

    /// Initial buffer content, `\n`-separated lines.
    pub initial_buffer: String,

    /// Initial cursor position as `(row, col)`, 0-based byte-col.
    #[serde(default)]
    pub initial_cursor: (usize, usize),

    /// Vim-key notation to replay, e.g. `"wdw"` or `"i hello<Esc>"`.
    pub keys: String,

    /// Expected buffer content after replaying `keys`.
    pub expected_buffer: String,

    /// Optional expected cursor position after replaying `keys`.
    #[serde(default)]
    pub expected_cursor: Option<(usize, usize)>,

    /// Optional expected vim mode after replaying `keys`.
    ///
    /// Matches lowercase [`hjkl_engine::VimMode`] variant names:
    /// `"normal"`, `"insert"`, `"visual"`, `"visual_line"`, `"visual_block"`.
    /// (`"replace"` and `"command"` are internal engine modes not surfaced
    /// via `VimMode::public_mode()` and therefore unsupported here.)
    #[serde(default)]
    pub expected_mode: Option<String>,

    /// Optional `(register_char, expected_contents)` check.
    #[serde(default)]
    pub expected_register: Option<(char, String)>,
}

/// A collection of [`OracleCase`]s loaded from a single TOML corpus file.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Corpus {
    pub cases: Vec<OracleCase>,
}

/// Load a [`Corpus`] from a TOML file at `path`.
///
/// The file must use `[[cases]]` array-of-tables syntax. Returns an error
/// if the file cannot be read or fails to parse.
pub fn load_corpus(path: &Path) -> anyhow::Result<Corpus> {
    let text = std::fs::read_to_string(path)?;
    let corpus: Corpus = toml::from_str(&text)?;
    Ok(corpus)
}
