/// Describes what the caller should do after an ex command runs.
///
/// Variants that mutate editor state (substitute, goto-line, clear-highlight)
/// are applied in-place inside the dispatcher; everything else is returned so
/// the host loop can act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExEffect {
    /// The `:s/pat/rep/c` (or `/gc`) flag triggered interactive confirm-mode.
    ///
    /// The caller should enter a per-match prompt loop: display each match,
    /// ask `y/n/a/q/l`, then apply the accepted subset via
    /// [`hjkl_engine::apply_collected_matches`].
    SubstituteConfirm {
        /// All candidate matches in document order (low row first).
        matches: Vec<hjkl_engine::SubstituteMatch>,
    },
    /// Nothing happened (empty input or no-op effect).
    None,
    /// Save the current buffer to the current filename.
    Save,
    /// Save to a specific path (`:w <path>`). The caller updates its
    /// `filename` field so future `:w` writes there.
    SaveAs(String),
    /// Quit (`:q`, `:q!`, `:wq`, `:x`).
    Quit { force: bool, save: bool },
    /// Unknown command — caller should surface as an error toast.
    Unknown(String),
    /// Substitution finished — report replacement count and lines changed.
    Substituted { count: usize, lines_changed: usize },
    /// A no-op response for successful commands that don't need a side
    /// effect but should not be reported as unknown (e.g. `:noh`).
    Ok,
    /// Surface an informational message (single-line or unclassified multi-line).
    Info(String),
    /// Surface a titled multi-line listing (`:reg`, `:marks`, `:jumps`, `:changes`).
    ///
    /// The `title` is a static label used by the host to open a named info
    /// popup without brittle header-prefix string matching.
    InfoTitled {
        /// Short human-readable label for the popup window (e.g. `"registers"`).
        title: &'static str,
        /// Full body text, newline-separated.
        content: String,
    },
    /// Surface an error message (syntax error, bad pattern, …).
    Error(String),
    /// `:e <path>` / `:edit <path>` — open a different file in the current
    /// window. An empty `path` means reload the current buffer.
    EditFile { path: String, force: bool },
    /// `:bd[!]` / `:bw[!]` — close the current buffer.
    /// `wipe = true` for `:bwipeout`; `force = true` when `!` was given.
    BufferDelete { force: bool, wipe: bool },
    /// `:put [{reg}]` / `:pu [{reg}]` — paste register contents as a new
    /// line below (or above when `above = true`) the cursor.
    PutRegister { reg: char, above: bool },
    /// `:saveas {path}` / `:sav {path}` — write buffer to `path` AND rename
    /// the buffer identity so future `:w` writes there.
    /// Distinct from `SaveAs` (`:w <path>`) which writes elsewhere but keeps
    /// the buffer's own filename unchanged.
    SaveAndRename { path: String },
    /// `:file {name}` — rename the current buffer in-memory without writing.
    RenameBuffer { name: String },
    /// `:cd [{path}]` — change the working directory. An empty path means
    /// `$HOME`. The directory change is applied inside the handler; the new
    /// path is surfaced so the host can update its status-line / title.
    Cwd(String),
    /// `:redraw[!]` — signal the host to repaint on the next frame.
    /// When `clear` is `true` (`:redraw!`) the host should clear the terminal
    /// before repainting. `:redraw` (no `!`) requests a plain repaint without
    /// clearing.
    Redraw { clear: bool },
}
