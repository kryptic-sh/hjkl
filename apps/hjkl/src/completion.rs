//! Completion popup — widget data model.
//!
//! `Completion` is the data half; `render::completion_popup` is the view half.

/// What kind of symbol this completion item represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    Other,
}

impl CompletionKind {
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

/// Active completion popup state.
#[derive(Debug, Clone)]
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
}

impl Completion {
    /// Create a new popup anchored at `(anchor_row, anchor_col)` with the
    /// given item list. All items are immediately visible (empty prefix).
    pub fn new(anchor_row: usize, anchor_col: usize, items: Vec<CompletionItem>) -> Self {
        let visible: Vec<usize> = (0..items.len()).collect();
        Self {
            anchor_row,
            anchor_col,
            all_items: items,
            visible,
            selected: 0,
            prefix: String::new(),
        }
    }

    /// Refilter visible items using a subsequence (case-insensitive) match
    /// against `prefix`. Resets `selected` to 0.
    pub fn set_prefix(&mut self, prefix: &str) {
        self.prefix = prefix.to_string();
        self.visible = self
            .all_items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let haystack = item
                    .filter_text
                    .as_deref()
                    .unwrap_or(&item.label)
                    .to_lowercase();
                subseq_match(&haystack, &prefix.to_lowercase())
            })
            .map(|(idx, _)| idx)
            .collect();
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

/// Returns true when every character of `needle` appears in `haystack`
/// in order (case already folded by the caller).
fn subseq_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut hiter = haystack.chars();
    for nc in needle.chars() {
        if !hiter.any(|hc| hc == nc) {
            return false;
        }
    }
    true
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
        filter_text: src.filter_text.clone(),
        // sort_text deferred; server order preserved (see struct comment).
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
}
