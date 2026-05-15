/// Describes what the caller should do after an ex command runs.
///
/// Variants that mutate editor state (substitute, goto-line, clear-highlight)
/// are applied in-place inside the dispatcher; everything else is returned so
/// the host loop can act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExEffect {
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
    /// Surface an informational message.
    Info(String),
    /// Surface an error message (syntax error, bad pattern, …).
    Error(String),
    /// `:e <path>` / `:edit <path>` — open a different file in the current
    /// window. An empty `path` means reload the current buffer.
    EditFile { path: String, force: bool },
    /// `:r <path>` / `:read <path>` — insert file contents below the
    /// cursor row.
    ReadFile { path: String },
    /// `:bd[!]` / `:bw[!]` — close the current buffer.
    /// `wipe = true` for `:bwipeout`; `force = true` when `!` was given.
    BufferDelete { force: bool, wipe: bool },
}
