//! Completion popup data model for the hjkl editor stack.
//!
//! `Completion` is the data half; the TUI renderer (`hjkl-completion-tui`)
//! is the view half. This crate has no UI dependencies.
//!
//! # Example
//!
//! ```
//! use hjkl_completion::{Completion, CompletionItem};
//!
//! let items = vec![CompletionItem::new("println")];
//! let popup = Completion::new(0, 0, items);
//! assert_eq!(popup.visible.len(), 1);
//! assert!(!popup.is_empty());
//! ```

/// What kind of symbol this completion item represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum CompletionKind {
    Function,
    Method,
    Variable,
    Field,
    Class,
    Module,
    Interface,
    Enum,
    Constant,
    Property,
    Snippet,
    Keyword,
    File,
    Folder,
    #[default]
    Other,
}

impl CompletionKind {
    /// Single-character icon for this completion kind, suitable for display
    /// in a narrow gutter column.
    pub fn icon(self) -> char {
        match self {
            CompletionKind::Function | CompletionKind::Method => '\u{0192}', // ƒ
            CompletionKind::Variable => 'v',
            CompletionKind::Field | CompletionKind::Property => '\u{00B7}', // ·
            CompletionKind::Class | CompletionKind::Interface => 'C',
            CompletionKind::Module => 'M',
            CompletionKind::Enum => 'E',
            CompletionKind::Constant => 'k',
            CompletionKind::Snippet => '\u{25C6}', // ◆
            CompletionKind::Keyword => 'K',
            CompletionKind::File | CompletionKind::Folder => '\u{25F0}', // ◰
            CompletionKind::Other => '\u{00B7}',                         // ·
        }
    }
}

/// A single item in a completion list.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub kind: CompletionKind,
    /// Text to insert; may equal `label` for plain word completions.
    pub insert_text: String,
    /// Text used for fuzzy matching (often the same as `label`).
    pub filter_text: Option<String>,
    // sort_text is parsed from the server but ordering by it is deferred to
    // a follow-up; server-provided order is preserved as-is in Phase 4.
}

impl CompletionItem {
    /// Construct a minimal item from a label, using `Other` as the kind and
    /// `label` as the `insert_text`.
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let insert_text = label.clone();
        Self {
            label,
            detail: None,
            kind: CompletionKind::Other,
            insert_text,
            filter_text: None,
        }
    }
}

impl Default for CompletionItem {
    fn default() -> Self {
        Self::new("")
    }
}

/// Active completion popup state.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Completion {
    /// Cursor row (0-based) when the popup was opened.
    pub anchor_row: usize,
    /// Cursor column (0-based) when the popup was opened.
    pub anchor_col: usize,
    /// All items returned by the server (unfiltered).
    pub all_items: Vec<CompletionItem>,
    /// Indices into `all_items` that survive the current `prefix` filter.
    pub visible: Vec<usize>,
    /// Selected index into `visible`.
    pub selected: usize,
    /// Prefix the user has typed since the popup was opened.
    pub prefix: String,
    /// Whether the renderer last drew the popup *above* the anchor line
    /// (flipped) rather than below it. Set by the view each frame via
    /// [`Completion::note_flip`]; consumed by [`Completion::cycle_down`] /
    /// [`Completion::cycle_up`] so cursor-key navigation always moves the
    /// highlight in the on-screen direction the user pressed. `Cell` so the
    /// view can record it through a shared `&Completion` borrow.
    flipped: std::cell::Cell<bool>,
}

impl Completion {
    /// Create a new popup anchored at `(anchor_row, anchor_col)` with the
    /// given item list. All items are immediately visible (empty prefix).
    ///
    /// # Example
    ///
    /// ```
    /// use hjkl_completion::{Completion, CompletionItem};
    ///
    /// let popup = Completion::new(1, 5, vec![CompletionItem::new("foo")]);
    /// assert_eq!(popup.anchor_row, 1);
    /// assert_eq!(popup.anchor_col, 5);
    /// assert_eq!(popup.visible.len(), 1);
    /// ```
    pub fn new(anchor_row: usize, anchor_col: usize, items: Vec<CompletionItem>) -> Self {
        let visible: Vec<usize> = (0..items.len()).collect();
        Self {
            anchor_row,
            anchor_col,
            all_items: items,
            visible,
            selected: 0,
            prefix: String::new(),
            flipped: std::cell::Cell::new(false),
        }
    }

    /// Refilter visible items using a case-insensitive subsequence match
    /// against `prefix`, **ranked by match quality** (best first): an exact
    /// match outranks a prefix match, which outranks a contiguous run, which
    /// outranks a scattered subsequence; shorter candidates and earlier / more
    /// word-boundary-aligned matches score higher. Ties keep the original
    /// (server-provided) order for stability. Resets `selected` to 0 so the
    /// best match is auto-selected.
    pub fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_string();
        let needle = prefix.to_lowercase();
        let mut scored: Vec<(usize, i32)> = self
            .all_items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| {
                let haystack = item
                    .filter_text
                    .as_deref()
                    .unwrap_or(&item.label)
                    .to_lowercase();
                match_score(&haystack, &needle).map(|score| (idx, score))
            })
            .collect();
        // Higher score first; on a tie fall back to the original index so the
        // sort is stable and the server's preferred ordering is preserved.
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self.visible = scored.into_iter().map(|(idx, _)| idx).collect();
        self.selected = 0;
    }

    /// Move selection down one step, wrapping at the end.
    pub fn select_next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.visible.len();
    }

    /// Move selection up one step, wrapping at the start.
    pub fn select_prev(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.visible.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    /// Record whether the view drew this popup flipped (above the anchor).
    /// Called by the renderer each frame; navigation then mirrors accordingly.
    pub fn note_flip(&self, flipped: bool) {
        self.flipped.set(flipped);
    }

    /// `true` when the popup was last rendered above the anchor line.
    pub fn is_flipped(&self) -> bool {
        self.flipped.get()
    }

    /// Move the highlight one row *down on screen*, regardless of orientation.
    /// When not flipped, visual order == logical order, so this is `select_next`.
    /// When flipped, the list is drawn inverted (best match at the bottom), so
    /// moving down visually means stepping to the previous logical item.
    pub fn cycle_down(&mut self) {
        if self.flipped.get() {
            self.select_prev();
        } else {
            self.select_next();
        }
    }

    /// Move the highlight one row *up on screen*, regardless of orientation.
    /// Inverse of [`Self::cycle_down`].
    pub fn cycle_up(&mut self) {
        if self.flipped.get() {
            self.select_next();
        } else {
            self.select_prev();
        }
    }

    /// Return the currently selected item, if any.
    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.visible
            .get(self.selected)
            .and_then(|&idx| self.all_items.get(idx))
    }

    /// True when no items match the current prefix — popup should auto-dismiss.
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }
}

impl Default for Completion {
    fn default() -> Self {
        Self::new(0, 0, Vec::new())
    }
}

/// Fuzzy match score for `needle` against `haystack` (both already
/// case-folded by the caller). Returns `None` when `needle` is not a
/// subsequence of `haystack`; otherwise a score where **higher is better**.
///
/// Heuristics, roughly in order of weight:
/// - exact equality is the strongest signal,
/// - a prefix match (`haystack` starts with `needle`) is next,
/// - contiguous matched runs beat scattered ones (gaps are penalized),
/// - matches at a word boundary (start, or after `_`) earn a bonus,
/// - an earlier first match and a shorter candidate score higher.
///
/// An empty `needle` matches everything with a neutral score, leaving the
/// original ordering untouched.
fn match_score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }

    let h: Vec<char> = haystack.chars().collect();
    let mut needle_iter = needle.chars();
    let mut want = needle_iter.next();
    let mut score: i32 = 0;
    let mut first_match: Option<usize> = None;
    let mut prev_match: Option<usize> = None;

    for (i, &hc) in h.iter().enumerate() {
        let Some(nc) = want else { break };
        if hc == nc {
            if first_match.is_none() {
                first_match = Some(i);
            }
            match prev_match {
                Some(p) if p + 1 == i => score += 15,   // contiguous run
                Some(p) => score -= (i - p - 1) as i32, // gap penalty
                None => {}
            }
            // Word-boundary bonus: start of string or right after `_`.
            if i == 0 || h[i - 1] == '_' {
                score += 10;
            }
            prev_match = Some(i);
            want = needle_iter.next();
        }
    }

    // Not all needle chars were consumed → not a subsequence.
    if want.is_some() {
        return None;
    }

    if let Some(f) = first_match {
        score -= f as i32; // earlier first match is better
    }
    if h == needle.chars().collect::<Vec<_>>() {
        score += 1000; // exact match
    } else if haystack.starts_with(needle) {
        score += 100; // prefix match
    }
    score -= (h.len() as i32) / 4; // mild preference for shorter candidates
    Some(score)
}

// ── Map lsp_types completion-item kinds ──────────────────────────────────────

/// Convert an `lsp_types::CompletionItemKind` to our `CompletionKind`.
pub fn kind_from_lsp(k: Option<lsp_types::CompletionItemKind>) -> CompletionKind {
    use lsp_types::CompletionItemKind as K;
    match k {
        Some(K::FUNCTION) => CompletionKind::Function,
        Some(K::METHOD) => CompletionKind::Method,
        Some(K::VARIABLE) => CompletionKind::Variable,
        Some(K::FIELD) => CompletionKind::Field,
        Some(K::CLASS) => CompletionKind::Class,
        Some(K::MODULE) => CompletionKind::Module,
        Some(K::INTERFACE) => CompletionKind::Interface,
        Some(K::ENUM) => CompletionKind::Enum,
        Some(K::CONSTANT) | Some(K::ENUM_MEMBER) => CompletionKind::Constant,
        Some(K::PROPERTY) => CompletionKind::Property,
        Some(K::SNIPPET) => CompletionKind::Snippet,
        Some(K::KEYWORD) => CompletionKind::Keyword,
        Some(K::FILE) => CompletionKind::File,
        Some(K::FOLDER) => CompletionKind::Folder,
        _ => CompletionKind::Other,
    }
}

/// Convert an `lsp_types::CompletionItem` to our local `CompletionItem`.
pub fn item_from_lsp(src: lsp_types::CompletionItem) -> CompletionItem {
    let insert_text = match src.text_edit.as_ref() {
        Some(lsp_types::CompletionTextEdit::Edit(te)) => te.new_text.clone(),
        Some(lsp_types::CompletionTextEdit::InsertAndReplace(ite)) => ite.new_text.clone(),
        None => src.insert_text.clone().unwrap_or_else(|| src.label.clone()),
    };
    CompletionItem {
        label: src.label.clone(),
        detail: src.detail.clone(),
        kind: kind_from_lsp(src.kind),
        insert_text,
        filter_text: src.filter_text,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(label: &str) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            detail: None,
            kind: CompletionKind::Other,
            insert_text: label.to_string(),
            filter_text: None,
        }
    }

    fn popup(labels: &[&str]) -> Completion {
        Completion::new(0, 0, labels.iter().map(|l| make_item(l)).collect())
    }

    #[test]
    fn set_prefix_filters_with_subseq_match() {
        let mut c = popup(&["foo_bar", "foobar", "baz"]);
        c.set_prefix("fb");
        // "foo_bar" → f..b.. ✓   "foobar" → f..b.. ✓   "baz" → no f ✗
        assert_eq!(c.visible.len(), 2, "visible: {:?}", c.visible);
    }

    #[test]
    fn set_prefix_case_insensitive() {
        let mut c = popup(&["FooBar", "foobar"]);
        c.set_prefix("FB");
        assert_eq!(c.visible.len(), 2);
    }

    #[test]
    fn set_prefix_ranks_exact_match_first() {
        // The exact keyword "let" must outrank scattered subsequence matches
        // like "STATUS_LINE_HEIGHT" (l…e…t) and prefix matches like "letter".
        let mut c = popup(&["STATUS_LINE_HEIGHT", "letter", "let", "delete"]);
        c.set_prefix("let");
        let ranked: Vec<&str> = c
            .visible
            .iter()
            .map(|&i| c.all_items[i].label.as_str())
            .collect();
        assert_eq!(ranked.first(), Some(&"let"), "ranked: {ranked:?}");
        // "letter" (prefix match) must beat the scattered ones.
        let letter_pos = ranked.iter().position(|&l| l == "letter").unwrap();
        let status_pos = ranked
            .iter()
            .position(|&l| l == "STATUS_LINE_HEIGHT")
            .unwrap();
        assert!(
            letter_pos < status_pos,
            "prefix match must rank above scattered: {ranked:?}"
        );
    }

    #[test]
    fn set_prefix_prefers_shorter_on_prefix_tie() {
        // Both start with "in"; the shorter identifier should rank first.
        let mut c = popup(&["instantiate", "in"]);
        c.set_prefix("in");
        let first = c.all_items[c.visible[0]].label.as_str();
        assert_eq!(first, "in");
    }

    #[test]
    fn set_prefix_empty_resets_to_all_items() {
        let mut c = popup(&["alpha", "beta", "gamma"]);
        c.set_prefix("alp");
        assert_eq!(c.visible.len(), 1);
        c.set_prefix("");
        assert_eq!(c.visible.len(), 3);
    }

    #[test]
    fn select_next_wraps_at_end() {
        let mut c = popup(&["a", "b", "c"]);
        c.selected = 2;
        c.select_next();
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn select_prev_wraps_at_start() {
        let mut c = popup(&["a", "b", "c"]);
        c.selected = 0;
        c.select_prev();
        assert_eq!(c.selected, 2);
    }

    #[test]
    fn cycle_matches_logical_direction_when_not_flipped() {
        let mut c = popup(&["a", "b", "c"]);
        c.note_flip(false);
        assert_eq!(c.selected, 0);
        c.cycle_down(); // down on screen == next logical item
        assert_eq!(c.selected, 1);
        c.cycle_up();
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn cycle_inverts_logical_direction_when_flipped() {
        // Flipped: best match (logical 0) is drawn at the BOTTOM, so moving the
        // highlight down on screen must step to the *previous* logical item
        // (wrapping to the worst match), and up steps to the next-best.
        let mut c = popup(&["a", "b", "c"]);
        c.note_flip(true);
        assert_eq!(c.selected, 0);
        c.cycle_up(); // up on screen, flipped == next logical item
        assert_eq!(c.selected, 1);
        c.cycle_up();
        assert_eq!(c.selected, 2);
        c.cycle_down(); // down on screen, flipped == prev logical item
        assert_eq!(c.selected, 1);
        // From the best match, down on screen wraps past the bottom edge.
        c.selected = 0;
        c.cycle_down();
        assert_eq!(c.selected, 2);
    }

    #[test]
    fn is_empty_after_no_match_filter() {
        let mut c = popup(&["alpha", "beta"]);
        c.set_prefix("xyz");
        assert!(c.is_empty());
    }

    #[test]
    fn selected_item_returns_correct_item() {
        let mut c = popup(&["alpha", "beta", "gamma"]);
        c.set_prefix("bet");
        // Only "beta" survives.
        assert_eq!(c.visible.len(), 1);
        assert_eq!(c.selected_item().map(|i| i.label.as_str()), Some("beta"));
    }

    #[test]
    fn default_completion_is_empty() {
        let c = Completion::default();
        assert!(c.is_empty());
        assert_eq!(c.anchor_row, 0);
        assert_eq!(c.anchor_col, 0);
    }

    #[test]
    fn completion_item_new_sets_insert_text_from_label() {
        let item = CompletionItem::new("my_fn");
        assert_eq!(item.label, "my_fn");
        assert_eq!(item.insert_text, "my_fn");
        assert!(matches!(item.kind, CompletionKind::Other));
    }

    #[test]
    fn completion_kind_icon_coverage() {
        assert_eq!(CompletionKind::Function.icon(), '\u{0192}');
        assert_eq!(CompletionKind::Snippet.icon(), '\u{25C6}');
        assert_eq!(CompletionKind::Other.icon(), '\u{00B7}');
    }
}
