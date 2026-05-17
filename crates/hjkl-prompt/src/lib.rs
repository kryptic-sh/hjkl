//! Renderer-agnostic ex/search prompt bar state machine.
//!
//! Provides [`PromptState`] — the data model for the `:`, `/`, and `?` prompt
//! bars.  No rendering types are referenced; the TUI adapter lives in
//! `hjkl-prompt-tui`.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_prompt::{PromptState, PromptKind};
//!
//! let mut prompt = PromptState::new(PromptKind::Command);
//! assert!(prompt.is_collecting());
//! assert_eq!(prompt.text(), "");
//! ```

use hjkl_engine::{CursorShape, Input as EngineInput, Key as EngineKey, VimMode};
use hjkl_form::TextFieldEditor;

// ── PromptKind ────────────────────────────────────────────────────────────────

/// Which prompt bar is active.
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromptKind {
    /// `:` ex command prompt.
    Command,
    /// `/` forward incremental search.
    SearchForward,
    /// `?` backward incremental search.
    SearchBackward,
}

impl PromptKind {
    /// The leading character displayed in the status line (`:`, `/`, or `?`).
    ///
    /// ```rust
    /// use hjkl_prompt::PromptKind;
    ///
    /// assert_eq!(PromptKind::Command.prefix_char(), ':');
    /// assert_eq!(PromptKind::SearchForward.prefix_char(), '/');
    /// assert_eq!(PromptKind::SearchBackward.prefix_char(), '?');
    /// ```
    pub fn prefix_char(&self) -> char {
        match self {
            PromptKind::Command => ':',
            PromptKind::SearchForward => '/',
            PromptKind::SearchBackward => '?',
        }
    }
}

// ── CommandCompletion ─────────────────────────────────────────────────────────

/// Active wildmenu completion state for a command-line prompt.
///
/// `None` outside completion (no Tab pressed yet, or after acceptance/cancel).
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct CommandCompletion {
    /// Original typed text the user can revert to with `<Esc>`.
    pub original: String,
    /// Sorted, dedup'd candidate strings.
    pub candidates: Vec<String>,
    /// Currently selected candidate index, or `None` on initial Tab when
    /// we replaced with the longest common prefix (no specific selection yet).
    pub selected: Option<usize>,
    /// Byte range in the field text that the candidate replaces.
    pub replace_range: std::ops::Range<usize>,
}

impl CommandCompletion {
    /// Construct a new `CommandCompletion`.
    pub fn new(
        original: String,
        candidates: Vec<String>,
        replace_range: std::ops::Range<usize>,
    ) -> Self {
        Self {
            original,
            candidates,
            selected: None,
            replace_range,
        }
    }
}

// ── PromptOutcome ─────────────────────────────────────────────────────────────

/// Result returned by [`PromptState::handle_key`].
///
/// `#[non_exhaustive]` — new variants may be added in minor releases.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PromptOutcome {
    /// The prompt is still active; caller should re-render.
    Continue,
    /// The prompt was submitted with this text.
    Submit(String),
    /// The prompt was cancelled (Esc / Backspace on empty).
    Cancel,
    /// A Tab/S-Tab was pressed; caller should drive `advance_completion`.
    TabForward,
    /// A Tab/S-Tab was pressed; caller should drive `advance_completion`.
    TabBackward,
    /// History navigation (`<C-p>`/Up or `<C-n>`/Down).
    HistoryPrev,
    /// History navigation (`<C-p>`/Up or `<C-n>`/Down).
    HistoryNext,
    /// Any other key was handled and the field text may have changed.
    Dirty,
}

// ── PromptState ───────────────────────────────────────────────────────────────

/// All state needed for a single active prompt bar (`:`, `/`, or `?`).
///
/// The prompt wraps a [`TextFieldEditor`] so vim motions (`h`/`l`/`w`/`b`/
/// `dw`/`diw`/…) work inside the prompt.
///
/// `#[non_exhaustive]` — new fields may be added in minor releases.
#[non_exhaustive]
pub struct PromptState {
    /// Which prompt kind is active.
    pub kind: PromptKind,
    /// The underlying vim-modal text field.
    pub field: TextFieldEditor,
    /// Active wildmenu completion state (Command prompts only).
    pub completion: Option<CommandCompletion>,
    /// Index into the history ring while `<C-p>`/`<C-n>` recall is active.
    /// `None` = not scrolling history.
    pub history_index: Option<usize>,
    /// The text the user had typed before the first `<C-p>` press —
    /// restored on `<C-n>` past the most-recent entry.
    pub user_input: Option<String>,
}

impl PromptState {
    /// Create a new prompt of the given kind, in Insert mode with cursor at end.
    ///
    /// ```rust
    /// use hjkl_prompt::{PromptState, PromptKind};
    ///
    /// let p = PromptState::new(PromptKind::Command);
    /// assert!(p.is_collecting());
    /// ```
    pub fn new(kind: PromptKind) -> Self {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        Self {
            kind,
            field,
            completion: None,
            history_index: None,
            user_input: None,
        }
    }

    /// Create a new Command prompt with `prefill` pre-typed and cursor at end.
    ///
    /// Used by the visual-mode `:` interceptor to seed `'<,'>`.
    ///
    /// ```rust
    /// use hjkl_prompt::{PromptState, PromptKind};
    ///
    /// let p = PromptState::with_prefill(PromptKind::Command, "'<,'>");
    /// assert_eq!(p.text(), "'<,'>");
    /// ```
    pub fn with_prefill(kind: PromptKind, prefill: &str) -> Self {
        let mut field = TextFieldEditor::new(true);
        field.enter_insert_at_end();
        for c in prefill.chars() {
            let input = EngineInput {
                key: EngineKey::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            };
            field.handle_input(input);
        }
        Self {
            kind,
            field,
            completion: None,
            history_index: None,
            user_input: None,
        }
    }

    /// Returns `true` while the prompt is open (always true for a live `PromptState`).
    pub fn is_collecting(&self) -> bool {
        true
    }

    /// Current text content of the field.
    pub fn text(&self) -> String {
        self.field.text()
    }

    /// Current cursor position `(row, col)`.
    pub fn cursor(&self) -> (usize, usize) {
        self.field.cursor()
    }

    /// Current vim mode of the inner field.
    pub fn vim_mode(&self) -> VimMode {
        self.field.vim_mode()
    }

    /// Resolve the terminal cursor shape for this prompt.
    ///
    /// Insert mode → `Bar`; everything else → `Block`.
    ///
    /// ```rust
    /// use hjkl_prompt::{PromptState, PromptKind};
    /// use hjkl_engine::CursorShape;
    ///
    /// let p = PromptState::new(PromptKind::Command);
    /// // New prompt starts in Insert mode.
    /// assert_eq!(p.cursor_shape(), CursorShape::Bar);
    /// ```
    pub fn cursor_shape(&self) -> CursorShape {
        match self.field.vim_mode() {
            VimMode::Insert => CursorShape::Bar,
            _ => CursorShape::Block,
        }
    }

    /// Handle a raw [`EngineInput`] and return what the caller should do next.
    ///
    /// Handles `Enter` (submit), `Esc` (cancel or mode switch), `Backspace` on
    /// empty (cancel), and delegates everything else to the inner
    /// [`TextFieldEditor`].
    ///
    /// Tab/S-Tab handling is left to the caller — they should call
    /// [`Self::advance_completion`] via the returned [`PromptOutcome`].
    pub fn handle_input(&mut self, input: EngineInput) -> PromptOutcome {
        if input.key == EngineKey::Enter {
            let text = self.field.text();
            return PromptOutcome::Submit(text);
        }

        if input.key == EngineKey::Esc {
            if let Some(comp) = self.completion.take() {
                // Revert field text to the original typed text.
                set_field_text(&mut self.field, &comp.original);
                return PromptOutcome::Continue;
            }
            if self.field.text().is_empty() {
                return PromptOutcome::Cancel;
            }
            if self.field.vim_mode() == VimMode::Insert {
                self.field.enter_normal();
                return PromptOutcome::Continue;
            }
            return PromptOutcome::Cancel;
        }

        // Backspace on empty dismisses prompt.
        if input.key == EngineKey::Backspace && self.field.text().is_empty() {
            return PromptOutcome::Cancel;
        }

        // Any non-history key resets history navigation state.
        if self.history_index.is_some() {
            self.history_index = None;
            self.user_input = None;
        }

        // Any other key while completion is active: commit current candidate.
        if self.completion.is_some() {
            self.completion = None;
        }

        let dirty = self.field.handle_input(input);
        if dirty {
            PromptOutcome::Dirty
        } else {
            PromptOutcome::Continue
        }
    }

    /// Navigate to a history entry by index (or back to user input when `None`).
    ///
    /// `history` is the caller's history ring. `idx` is the new
    /// `history_index` to apply (`None` = restore user input).
    pub fn apply_history_nav(&mut self, history: &[String], idx: Option<usize>) {
        if self.history_index.is_none() {
            // Save current typed input on first history nav.
            self.user_input = Some(self.field.text());
        }
        self.history_index = idx;
        let text = match idx {
            Some(i) => history.get(i).cloned().unwrap_or_default(),
            None => self.user_input.clone().unwrap_or_default(),
        };
        set_field_text(&mut self.field, &text);
    }

    /// Advance (or initialize) wildmenu completion state.
    ///
    /// `forward=true` means Tab (next); `false` means S-Tab (prev).
    ///
    /// `comp` is the new [`CommandCompletion`] to install when starting a fresh
    /// completion cycle. When cycling through existing candidates, pass `None`
    /// for `comp` — the existing state is updated in-place.
    pub fn advance_completion(&mut self, comp: Option<CommandCompletion>, forward: bool) {
        if let Some(new_comp) = comp {
            // Install fresh completion state (caller computed it).
            self.completion = Some(new_comp);
        }

        let Some(state) = self.completion.as_mut() else {
            return;
        };
        if state.candidates.is_empty() {
            return;
        }
        let n = state.candidates.len();
        let new_idx = match state.selected {
            None => {
                if forward {
                    0
                } else {
                    n - 1
                }
            }
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        };
        state.selected = Some(new_idx);
        let candidate = state.candidates[new_idx].clone();
        let new_text = format!(
            "{}{}",
            &self.field.text()[..state.replace_range.start],
            candidate
        );
        let new_end = state.replace_range.start + candidate.len();
        state.replace_range = state.replace_range.start..new_end;
        set_field_text(&mut self.field, &new_text);
    }
}

impl Default for PromptState {
    fn default() -> Self {
        Self::new(PromptKind::Command)
    }
}

// ── History helpers ───────────────────────────────────────────────────────────

/// Push `entry` into a history ring (cap 100, skip consecutive duplicates).
///
/// Empty entries are silently ignored.
///
/// ```rust
/// use hjkl_prompt::push_history;
///
/// let mut ring: Vec<String> = Vec::new();
/// push_history(&mut ring, "ls");
/// push_history(&mut ring, "ls"); // consecutive duplicate → skipped
/// push_history(&mut ring, "cd /tmp");
/// assert_eq!(ring, vec!["ls", "cd /tmp"]);
/// ```
pub fn push_history(ring: &mut Vec<String>, entry: &str) {
    if entry.is_empty() {
        return;
    }
    if ring.last().is_some_and(|last| last == entry) {
        return;
    }
    ring.push(entry.to_string());
    const HISTORY_CAP: usize = 100;
    if ring.len() > HISTORY_CAP {
        ring.remove(0);
    }
}

/// Compute the new `history_index` for a `<C-p>` (prev) navigation step.
///
/// ```rust
/// use hjkl_prompt::history_prev;
///
/// assert_eq!(history_prev(None, 3), Some(2)); // first C-p → oldest
/// assert_eq!(history_prev(Some(2), 3), Some(1));
/// assert_eq!(history_prev(Some(0), 3), Some(0)); // clamp at oldest
/// ```
pub fn history_prev(current: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match current {
        None => Some(len - 1),
        Some(0) => Some(0), // clamp at oldest
        Some(i) => Some(i - 1),
    }
}

/// Compute the new `history_index` for a `<C-n>` (next) navigation step.
///
/// ```rust
/// use hjkl_prompt::history_next;
///
/// assert_eq!(history_next(None, 3), None);   // already at tip
/// assert_eq!(history_next(Some(2), 3), None); // past newest → restore user input
/// assert_eq!(history_next(Some(0), 3), Some(1));
/// ```
pub fn history_next(current: Option<usize>, len: usize) -> Option<usize> {
    match current {
        None => None,
        Some(i) if i + 1 >= len => None, // past newest → restore user input
        Some(i) => Some(i + 1),
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Replace the full text of a `TextFieldEditor`, leaving cursor at end in
/// Insert mode.
fn set_field_text(field: &mut TextFieldEditor, text: &str) {
    field.set_text(text);
    field.enter_insert_at_end();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_prompt_is_collecting() {
        let p = PromptState::new(PromptKind::Command);
        assert!(p.is_collecting());
    }

    #[test]
    fn new_prompt_text_is_empty() {
        let p = PromptState::new(PromptKind::SearchForward);
        assert_eq!(p.text(), "");
    }

    #[test]
    fn with_prefill_sets_text() {
        let p = PromptState::with_prefill(PromptKind::Command, "'<,'>");
        assert_eq!(p.text(), "'<,'>");
    }

    #[test]
    fn cursor_shape_insert_is_bar() {
        let p = PromptState::new(PromptKind::Command);
        assert_eq!(p.cursor_shape(), CursorShape::Bar);
    }

    #[test]
    fn kind_prefix_chars() {
        assert_eq!(PromptKind::Command.prefix_char(), ':');
        assert_eq!(PromptKind::SearchForward.prefix_char(), '/');
        assert_eq!(PromptKind::SearchBackward.prefix_char(), '?');
    }

    #[test]
    fn handle_enter_submits_text() {
        let mut p = PromptState::with_prefill(PromptKind::Command, "write");
        let out = p.handle_input(EngineInput {
            key: EngineKey::Enter,
            ctrl: false,
            alt: false,
            shift: false,
        });
        assert_eq!(out, PromptOutcome::Submit("write".into()));
    }

    #[test]
    fn handle_esc_on_empty_cancels() {
        let mut p = PromptState::new(PromptKind::Command);
        let out = p.handle_input(EngineInput {
            key: EngineKey::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        });
        assert_eq!(out, PromptOutcome::Cancel);
    }

    #[test]
    fn handle_backspace_on_empty_cancels() {
        let mut p = PromptState::new(PromptKind::Command);
        let out = p.handle_input(EngineInput {
            key: EngineKey::Backspace,
            ctrl: false,
            alt: false,
            shift: false,
        });
        assert_eq!(out, PromptOutcome::Cancel);
    }

    #[test]
    fn push_history_skips_empty() {
        let mut ring: Vec<String> = Vec::new();
        push_history(&mut ring, "");
        assert!(ring.is_empty());
    }

    #[test]
    fn push_history_deduplicates_consecutive() {
        let mut ring: Vec<String> = Vec::new();
        push_history(&mut ring, "ls");
        push_history(&mut ring, "ls");
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn push_history_allows_non_consecutive_duplicates() {
        let mut ring: Vec<String> = Vec::new();
        push_history(&mut ring, "ls");
        push_history(&mut ring, "cd");
        push_history(&mut ring, "ls");
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn push_history_caps_at_100() {
        let mut ring: Vec<String> = Vec::new();
        for i in 0..110u32 {
            push_history(&mut ring, &format!("cmd{i}"));
        }
        assert_eq!(ring.len(), 100);
    }

    #[test]
    fn history_prev_from_none() {
        assert_eq!(history_prev(None, 3), Some(2));
    }

    #[test]
    fn history_prev_clamps_at_zero() {
        assert_eq!(history_prev(Some(0), 3), Some(0));
    }

    #[test]
    fn history_next_from_end_restores() {
        assert_eq!(history_next(Some(2), 3), None);
    }

    #[test]
    fn history_next_from_none_is_none() {
        assert_eq!(history_next(None, 3), None);
    }

    #[test]
    fn apply_history_nav_sets_text() {
        let mut p = PromptState::new(PromptKind::Command);
        let history = vec!["ls".to_string(), "cd /tmp".to_string()];
        p.apply_history_nav(&history, Some(1));
        assert_eq!(p.text(), "cd /tmp");
    }

    #[test]
    fn apply_history_nav_none_restores_user_input() {
        let mut p = PromptState::with_prefill(PromptKind::Command, "partial");
        let history = vec!["ls".to_string()];
        p.apply_history_nav(&history, Some(0));
        p.apply_history_nav(&history, None);
        assert_eq!(p.text(), "partial");
    }

    #[test]
    fn completion_new_has_no_selection() {
        let comp = CommandCompletion::new("w".into(), vec!["write".into(), "wall".into()], 0..1);
        assert!(comp.selected.is_none());
    }

    #[test]
    fn advance_completion_cycles_forward() {
        let mut p = PromptState::with_prefill(PromptKind::Command, "w");
        let comp = CommandCompletion::new("w".into(), vec!["write".into(), "wall".into()], 0..1);
        p.advance_completion(Some(comp), true);
        assert_eq!(p.completion.as_ref().unwrap().selected, Some(0));
        p.advance_completion(None, true);
        assert_eq!(p.completion.as_ref().unwrap().selected, Some(1));
        p.advance_completion(None, true);
        assert_eq!(p.completion.as_ref().unwrap().selected, Some(0)); // wrap
    }

    #[test]
    fn esc_with_completion_reverts_to_original() {
        let mut p = PromptState::with_prefill(PromptKind::Command, "write");
        let comp = CommandCompletion::new("w".into(), vec!["write".into(), "wall".into()], 0..5);
        p.completion = Some(comp);
        let out = p.handle_input(EngineInput {
            key: EngineKey::Esc,
            ctrl: false,
            alt: false,
            shift: false,
        });
        assert_eq!(out, PromptOutcome::Continue);
        assert_eq!(p.text(), "w"); // reverted to original
        assert!(p.completion.is_none());
    }

    #[test]
    fn default_is_command_kind() {
        let p = PromptState::default();
        assert_eq!(p.kind, PromptKind::Command);
    }
}
