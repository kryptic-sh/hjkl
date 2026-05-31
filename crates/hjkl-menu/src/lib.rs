//! Renderer-agnostic context menu model for the hjkl editor stack.
//!
//! Provides [`MenuAction`], [`MenuItem`], [`ContextMenu`] (open/close state +
//! keyboard navigation), and builder helpers for each menu zone
//! ([`build_code_menu`], [`build_status_line_menu`], [`build_split_border_menu`],
//! [`build_picker_menu`], [`build_tab_menu`]).
//!
//! No rendering dependencies — ratatui/floem adapters live in
//! `hjkl-menu-tui` / `hjkl-menu-gui`.
//!
//! # Quick start
//!
//! ```rust
//! use hjkl_menu::{build_code_menu, ContextMenu, MenuAction};
//!
//! let items = build_code_menu(true, false);
//! let mut menu = ContextMenu::new(items, (10, 5));
//! menu.move_down();
//! let action = menu.selected_action();
//! // action is Some(MenuAction::Copy) — the second selectable item.
//! assert_eq!(action, Some(MenuAction::Copy));
//! ```

#![forbid(unsafe_code)]

// ── MenuAction ────────────────────────────────────────────────────────────────

/// Each selectable item in the context menu maps to one of these variants.
///
/// `Separator` is a non-selectable divider rendered as a horizontal rule.
/// `Info` is a non-selectable informational header row (dimmed, not a rule).
///
/// `#[non_exhaustive]` — new action variants may be added in minor releases
/// without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MenuAction {
    // ── Clipboard ──────────────────────────────────────────────────────────
    Copy,
    Cut,
    Paste,
    // ── Tab management ─────────────────────────────────────────────────────
    TabClose,
    TabCloseOthers,
    TabCloseRight,
    TabCloseLeft,
    // ── LSP ────────────────────────────────────────────────────────────────
    LspGotoDefinition,
    LspGotoReferences,
    LspHover,
    LspRename,
    LspCodeActions,
    LspFormat,
    // ── Gutter / diagnostic menu ───────────────────────────────────────────
    /// Show the diagnostic(s) on the line under the pointer (#114 P6).
    DiagnosticDetail,
    // ── Gutter / git-hunk menu (#114 P6/P10, #115) ──────────────────────────
    /// Stage the git hunk under the pointer into the index.
    GitStageHunk,
    /// Unstage the git hunk under the pointer from the index.
    GitUnstageHunk,
    /// Revert the git hunk under the pointer, restoring the index baseline.
    GitRevertHunk,
    /// Show the git hunk under the pointer in a popup.
    GitShowHunk,
    // ── Status-line menu ───────────────────────────────────────────────────
    /// Restart the LSP server for the current buffer.
    LspRestart,
    /// Open the file picker (`<leader><space>`).
    OpenFilePicker,
    // ── Split-border menu ──────────────────────────────────────────────────
    /// Equalize all splits to 0.5 ratio.
    WindowEqualize,
    /// Close the focused window (`:close`).
    WindowClose,
    // ── Picker overlay menu ────────────────────────────────────────────────
    /// Open the focused picker row (same as Enter).
    PickerOpen,
    /// Open the focused picker row in a horizontal split.
    PickerOpenSplit,
    /// Open the focused picker row in a vertical split.
    PickerOpenVSplit,
    /// Open the focused picker row in a new tab.
    PickerOpenTab,
    /// Copy the focused picker row's path to the system clipboard.
    PickerCopyPath,
    // ── Visual decoration ──────────────────────────────────────────────────
    /// A non-selectable horizontal separator.
    Separator,
    /// A non-selectable informational header label.
    ///
    /// Rendered as dimmed text, not as a horizontal rule. Used for headers
    /// like "Filetype: rust" and "LSP: rust-analyzer" in the status-line menu.
    Info,
}

impl MenuAction {
    /// Returns `true` when this variant represents a non-selectable row
    /// (separator or info header).
    #[inline]
    pub fn is_inert(&self) -> bool {
        matches!(self, MenuAction::Separator | MenuAction::Info)
    }

    /// Dispatch `self` through a caller-supplied handler using the exhaustive
    /// [`MenuActionKind`] enum.
    ///
    /// Because [`MenuAction`] is `#[non_exhaustive]`, consumers that match on
    /// it directly must include a `_ => {}` wildcard arm.  Calling this method
    /// instead lets consumers match against [`MenuActionKind`] — which is
    /// **not** `#[non_exhaustive]` — and get a compile error when a new action
    /// variant is added without a handler.
    ///
    /// Returns `true` when the action was dispatched to a known variant,
    /// `false` when it was an unknown future variant and the handler was not
    /// called (callers can treat this as a no-op).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use hjkl_menu::{MenuAction, MenuActionKind};
    ///
    /// let mut copied = false;
    /// let handled = MenuAction::Copy.dispatch(|kind| match kind {
    ///     MenuActionKind::Copy => { copied = true; }
    ///     _ => {}
    /// });
    /// assert!(handled);
    /// assert!(copied);
    /// ```
    pub fn dispatch(&self, mut handler: impl FnMut(MenuActionKind)) -> bool {
        // `#[allow(unreachable_patterns)]` because from inside this crate all
        // MenuAction variants are known; the wildcard exists so this helper
        // stays future-proof for external consumers when new variants land.
        #[allow(unreachable_patterns)]
        match self {
            MenuAction::Copy => {
                handler(MenuActionKind::Copy);
                true
            }
            MenuAction::Cut => {
                handler(MenuActionKind::Cut);
                true
            }
            MenuAction::Paste => {
                handler(MenuActionKind::Paste);
                true
            }
            MenuAction::TabClose => {
                handler(MenuActionKind::TabClose);
                true
            }
            MenuAction::TabCloseOthers => {
                handler(MenuActionKind::TabCloseOthers);
                true
            }
            MenuAction::TabCloseRight => {
                handler(MenuActionKind::TabCloseRight);
                true
            }
            MenuAction::TabCloseLeft => {
                handler(MenuActionKind::TabCloseLeft);
                true
            }
            MenuAction::LspGotoDefinition => {
                handler(MenuActionKind::LspGotoDefinition);
                true
            }
            MenuAction::LspGotoReferences => {
                handler(MenuActionKind::LspGotoReferences);
                true
            }
            MenuAction::LspHover => {
                handler(MenuActionKind::LspHover);
                true
            }
            MenuAction::LspRename => {
                handler(MenuActionKind::LspRename);
                true
            }
            MenuAction::LspCodeActions => {
                handler(MenuActionKind::LspCodeActions);
                true
            }
            MenuAction::LspFormat => {
                handler(MenuActionKind::LspFormat);
                true
            }
            MenuAction::DiagnosticDetail => {
                handler(MenuActionKind::DiagnosticDetail);
                true
            }
            MenuAction::GitStageHunk => {
                handler(MenuActionKind::GitStageHunk);
                true
            }
            MenuAction::GitUnstageHunk => {
                handler(MenuActionKind::GitUnstageHunk);
                true
            }
            MenuAction::GitRevertHunk => {
                handler(MenuActionKind::GitRevertHunk);
                true
            }
            MenuAction::GitShowHunk => {
                handler(MenuActionKind::GitShowHunk);
                true
            }
            MenuAction::LspRestart => {
                handler(MenuActionKind::LspRestart);
                true
            }
            MenuAction::OpenFilePicker => {
                handler(MenuActionKind::OpenFilePicker);
                true
            }
            MenuAction::WindowEqualize => {
                handler(MenuActionKind::WindowEqualize);
                true
            }
            MenuAction::WindowClose => {
                handler(MenuActionKind::WindowClose);
                true
            }
            MenuAction::PickerOpen => {
                handler(MenuActionKind::PickerOpen);
                true
            }
            MenuAction::PickerOpenSplit => {
                handler(MenuActionKind::PickerOpenSplit);
                true
            }
            MenuAction::PickerOpenVSplit => {
                handler(MenuActionKind::PickerOpenVSplit);
                true
            }
            MenuAction::PickerOpenTab => {
                handler(MenuActionKind::PickerOpenTab);
                true
            }
            MenuAction::PickerCopyPath => {
                handler(MenuActionKind::PickerCopyPath);
                true
            }
            MenuAction::Separator => {
                handler(MenuActionKind::Separator);
                true
            }
            MenuAction::Info => {
                handler(MenuActionKind::Info);
                true
            }
            // Unknown future variant — no-op.
            _ => false,
        }
    }
}

/// Exhaustive view of a [`MenuAction`] for use in [`MenuAction::dispatch`]
/// callbacks.
///
/// Unlike [`MenuAction`] (which is `#[non_exhaustive]`), matching on this enum
/// produces a compile error when a new action variant is added without a
/// handler, ensuring consumers stay in sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuActionKind {
    // ── Clipboard ──────────────────────────────────────────────────────────
    Copy,
    Cut,
    Paste,
    // ── Tab management ─────────────────────────────────────────────────────
    TabClose,
    TabCloseOthers,
    TabCloseRight,
    TabCloseLeft,
    // ── LSP ────────────────────────────────────────────────────────────────
    LspGotoDefinition,
    LspGotoReferences,
    LspHover,
    LspRename,
    LspCodeActions,
    LspFormat,
    // ── Gutter / diagnostic menu ───────────────────────────────────────────
    DiagnosticDetail,
    // ── Gutter / git-hunk menu ──────────────────────────────────────────────
    GitStageHunk,
    GitUnstageHunk,
    GitRevertHunk,
    GitShowHunk,
    // ── Status-line menu ───────────────────────────────────────────────────
    LspRestart,
    OpenFilePicker,
    // ── Split-border menu ──────────────────────────────────────────────────
    WindowEqualize,
    WindowClose,
    // ── Picker overlay menu ────────────────────────────────────────────────
    PickerOpen,
    PickerOpenSplit,
    PickerOpenVSplit,
    PickerOpenTab,
    PickerCopyPath,
    // ── Visual decoration ──────────────────────────────────────────────────
    Separator,
    Info,
}

// ── MenuItem ──────────────────────────────────────────────────────────────────

/// One row in the context menu.
///
/// `#[non_exhaustive]` — new display fields may be added without a breaking
/// change. Construct via [`MenuItem::new`] or [`MenuItem::separator`].
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct MenuItem {
    /// Display label shown to the user.
    pub label: String,
    /// The action to invoke when this item is selected.
    pub action: MenuAction,
    /// Whether the item can be selected and invoked. Disabled items are
    /// rendered in a dimmed style and skipped by keyboard navigation.
    pub enabled: bool,
    /// Optional short-cut hint shown right-aligned (e.g. `"y"`, `"Ctrl+C"`).
    pub shortcut_hint: Option<String>,
}

impl MenuItem {
    /// Convenience constructor for an enabled item.
    ///
    /// ```rust
    /// use hjkl_menu::{MenuItem, MenuAction};
    ///
    /// let item = MenuItem::new("Copy", MenuAction::Copy, Some("y".to_string()));
    /// assert!(item.enabled);
    /// assert_eq!(item.label, "Copy");
    /// ```
    pub fn new(
        label: impl Into<String>,
        action: MenuAction,
        shortcut_hint: impl Into<Option<String>>,
    ) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
            shortcut_hint: shortcut_hint.into(),
        }
    }

    /// Convenience constructor for a separator row.
    ///
    /// ```rust
    /// use hjkl_menu::{MenuItem, MenuAction};
    ///
    /// let sep = MenuItem::separator();
    /// assert!(!sep.enabled);
    /// assert_eq!(sep.action, MenuAction::Separator);
    /// ```
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: MenuAction::Separator,
            enabled: false,
            shortcut_hint: None,
        }
    }

    /// Returns `true` when this item is selectable (enabled and not inert).
    #[inline]
    pub fn is_selectable(&self) -> bool {
        self.enabled && !self.action.is_inert()
    }
}

// ── ContextMenu ───────────────────────────────────────────────────────────────

/// Floating context menu state: items, highlight index, and anchor position.
///
/// `selected` always points to a row that is selectable (not a separator,
/// not disabled, not an info header). Navigation skips inert rows.
///
/// `#[non_exhaustive]` — new state fields may be added in minor releases.
#[non_exhaustive]
pub struct ContextMenu {
    /// All rows — separators and info headers included.
    pub items: Vec<MenuItem>,
    /// Index of the currently highlighted row.
    pub selected: usize,
    /// Screen position of the top-left corner `(col, row)`.
    pub anchor: (u16, u16),
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new(vec![], (0, 0))
    }
}

impl ContextMenu {
    /// Construct a new menu anchored at `(col, row)`.
    ///
    /// The initial `selected` advances past any leading inert rows.
    ///
    /// ```rust
    /// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
    ///
    /// let items = vec![
    ///     MenuItem::separator(),
    ///     MenuItem::new("Copy", MenuAction::Copy, None),
    /// ];
    /// let m = ContextMenu::new(items, (0, 0));
    /// assert_eq!(m.selected, 1, "must skip leading separator");
    /// ```
    pub fn new(items: Vec<MenuItem>, anchor: (u16, u16)) -> Self {
        let selected = items.iter().position(|it| it.is_selectable()).unwrap_or(0);
        Self {
            items,
            selected,
            anchor,
        }
    }

    /// Move the highlight one row up, skipping inert rows.
    /// Saturates at the first selectable item (does not wrap).
    ///
    /// ```rust
    /// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
    ///
    /// let items = vec![
    ///     MenuItem::new("Cut",  MenuAction::Cut,  None),
    ///     MenuItem::new("Copy", MenuAction::Copy, None),
    /// ];
    /// let mut m = ContextMenu::new(items, (0, 0));
    /// m.selected = 1;
    /// m.move_up();
    /// assert_eq!(m.selected, 0);
    /// m.move_up(); // at top — saturate
    /// assert_eq!(m.selected, 0);
    /// ```
    pub fn move_up(&mut self) {
        let current = self.selected;
        if current == 0 {
            return;
        }
        for idx in (0..current).rev() {
            if self.items[idx].is_selectable() {
                self.selected = idx;
                return;
            }
        }
    }

    /// Move the highlight one row down, skipping inert rows.
    /// Wraps to the first selectable item when already at the last.
    ///
    /// ```rust
    /// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
    ///
    /// let items = vec![
    ///     MenuItem::new("Cut",   MenuAction::Cut,   None),
    ///     MenuItem::separator(),
    ///     MenuItem::new("Paste", MenuAction::Paste, None),
    /// ];
    /// let mut m = ContextMenu::new(items, (0, 0));
    /// // Start at index 0 (Cut), move down → should land on Paste (idx 2).
    /// m.move_down();
    /// assert_eq!(m.selected, 2);
    /// // From last item, wrap back to first.
    /// m.move_down();
    /// assert_eq!(m.selected, 0);
    /// ```
    pub fn move_down(&mut self) {
        let len = self.items.len();
        let start = self.selected + 1;
        // Try below first.
        for idx in start..len {
            if self.items[idx].is_selectable() {
                self.selected = idx;
                return;
            }
        }
        // Wrap to top.
        for idx in 0..len {
            if self.items[idx].is_selectable() {
                self.selected = idx;
                return;
            }
        }
    }

    /// Return the action for the currently selected row.
    ///
    /// Returns `None` when the selected item is disabled or inert.
    ///
    /// ```rust
    /// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
    ///
    /// let items = vec![MenuItem::new("Copy", MenuAction::Copy, None)];
    /// let m = ContextMenu::new(items, (0, 0));
    /// assert_eq!(m.selected_action(), Some(MenuAction::Copy));
    /// ```
    pub fn selected_action(&self) -> Option<MenuAction> {
        let item = self.items.get(self.selected)?;
        if !item.is_selectable() {
            return None;
        }
        Some(item.action.clone())
    }

    /// Compute `(width, height)` of the popup box including border.
    ///
    /// Used by `hjkl-menu-tui` and the bounding-rect clamp helper.
    pub fn dimensions(&self) -> (u16, u16) {
        let content_w = self
            .items
            .iter()
            .map(|it| {
                if it.action.is_inert() {
                    return 0u16;
                }
                let hint_len = it
                    .shortcut_hint
                    .as_deref()
                    .map(|h| h.len() + 2)
                    .unwrap_or(0);
                (it.label.len() + hint_len) as u16
            })
            .max()
            .unwrap_or(8);

        // +4 = left-pad + right-pad inside the border + 2 border columns.
        let popup_w = content_w + 4;
        // One row per item, +2 for top/bottom border.
        let popup_h = self.items.len() as u16 + 2;
        (popup_w, popup_h)
    }

    /// Compute the bounding rectangle the menu occupies on screen.
    ///
    /// `(screen_w, screen_h)` is the full terminal size so the popup stays
    /// fully on-screen even when the anchor is near an edge.
    ///
    /// ```rust
    /// use hjkl_menu::{ContextMenu, MenuItem, MenuAction};
    ///
    /// // 6-item menu, anchor near bottom-right of a 80×24 terminal.
    /// let items: Vec<_> = (0..6)
    ///     .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
    ///     .collect();
    /// let m = ContextMenu::new(items, (75, 22));
    /// let (x, y, w, h) = m.bounding_rect(80, 24);
    /// assert!(x + w <= 80, "must not overflow right edge");
    /// assert!(y + h <= 24, "must not overflow bottom edge");
    /// ```
    pub fn bounding_rect(&self, screen_w: u16, screen_h: u16) -> (u16, u16, u16, u16) {
        let (popup_w, popup_h) = self.dimensions();
        let (ax, ay) = self.anchor;
        let x = ax.min(screen_w.saturating_sub(popup_w));
        let y = ay.min(screen_h.saturating_sub(popup_h));
        (x, y, popup_w, popup_h)
    }
}

// ── Menu builder helpers ──────────────────────────────────────────────────────

/// Build the context menu for a right-click in the Code or Gutter zone.
///
/// Cut / Copy are enabled only when a visual selection is active (`has_sel`).
/// LSP items are shown but disabled when no language server is attached
/// (`has_lsp = false`).
///
/// ```rust
/// use hjkl_menu::{build_code_menu, MenuAction};
///
/// let items = build_code_menu(true, true);
/// assert!(items.iter().any(|it| it.action == MenuAction::LspGotoDefinition));
/// ```
pub fn build_code_menu(has_sel: bool, has_lsp: bool) -> Vec<MenuItem> {
    vec![
        // ── Clipboard ──────────────────────────────────────────────────────
        MenuItem {
            label: "Cut".into(),
            action: MenuAction::Cut,
            enabled: has_sel,
            shortcut_hint: Some("x".into()),
        },
        MenuItem {
            label: "Copy".into(),
            action: MenuAction::Copy,
            enabled: has_sel,
            shortcut_hint: Some("y".into()),
        },
        MenuItem::new("Paste", MenuAction::Paste, Some("p".into())),
        // ── Separator ──────────────────────────────────────────────────────
        MenuItem::separator(),
        // ── LSP: navigation ────────────────────────────────────────────────
        MenuItem {
            label: "Go to Definition".into(),
            action: MenuAction::LspGotoDefinition,
            enabled: has_lsp,
            shortcut_hint: Some("gd".into()),
        },
        MenuItem {
            label: "Go to References".into(),
            action: MenuAction::LspGotoReferences,
            enabled: has_lsp,
            shortcut_hint: Some("gr".into()),
        },
        MenuItem {
            label: "Hover".into(),
            action: MenuAction::LspHover,
            enabled: has_lsp,
            shortcut_hint: Some("K".into()),
        },
        // ── Separator ──────────────────────────────────────────────────────
        MenuItem::separator(),
        // ── LSP: edits ─────────────────────────────────────────────────────
        MenuItem {
            label: "Rename Symbol".into(),
            action: MenuAction::LspRename,
            enabled: has_lsp,
            shortcut_hint: Some("<leader>rn".into()),
        },
        MenuItem {
            label: "Code Actions".into(),
            action: MenuAction::LspCodeActions,
            enabled: has_lsp,
            shortcut_hint: Some("<leader>ca".into()),
        },
        MenuItem {
            label: "Format Document".into(),
            action: MenuAction::LspFormat,
            enabled: has_lsp,
            shortcut_hint: Some(":LspFormat".into()),
        },
    ]
}

/// Which git-hunk state, if any, is under the gutter pointer. Drives whether the
/// gutter menu offers stage/revert (unstaged) or unstage (already staged).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GitHunkKind {
    /// No git hunk on this line.
    #[default]
    None,
    /// An unstaged change (index↔buffer) — stageable / revertable.
    Unstaged,
    /// A staged change (HEAD↔index) — unstageable.
    Staged,
}

/// Build the context menu for a right-click in the gutter / sign column
/// (#114 P6/P10, #115).
///
/// When a diagnostic sign is present on the clicked line (`has_diag`), the
/// menu leads with diagnostic-aware entries — "Show Diagnostic" (always usable
/// when a diagnostic is present) and "Code Actions" (enabled only when a
/// language server is attached, since quick-fixes come from the server).
///
/// The git section adapts to `git`: an [`GitHunkKind::Unstaged`] change offers
/// Stage / Revert / Show Hunk Diff; a [`GitHunkKind::Staged`] change offers
/// Unstage / Show Hunk Diff. Below a separator it falls through to the same
/// Code-zone actions so the gutter is never a dead-end.
///
/// ```rust
/// use hjkl_menu::{build_gutter_menu, GitHunkKind, MenuAction};
///
/// // Diagnostic present + LSP attached → leads with Show Diagnostic.
/// let items = build_gutter_menu(true, GitHunkKind::None, true, false);
/// assert_eq!(items[0].action, MenuAction::DiagnosticDetail);
///
/// // Unstaged hunk → Stage / Revert offered.
/// let un = build_gutter_menu(false, GitHunkKind::Unstaged, false, false);
/// assert!(un.iter().any(|it| it.action == MenuAction::GitStageHunk));
/// assert!(un.iter().any(|it| it.action == MenuAction::GitRevertHunk));
///
/// // Staged hunk → Unstage offered, no Stage/Revert.
/// let st = build_gutter_menu(false, GitHunkKind::Staged, false, false);
/// assert!(st.iter().any(|it| it.action == MenuAction::GitUnstageHunk));
/// assert!(!st.iter().any(|it| it.action == MenuAction::GitStageHunk));
///
/// // Nothing gutter-specific → plain Code menu.
/// let plain = build_gutter_menu(false, GitHunkKind::None, true, false);
/// assert!(!plain.iter().any(|it| it.action == MenuAction::DiagnosticDetail));
/// ```
pub fn build_gutter_menu(
    has_diag: bool,
    git: GitHunkKind,
    has_lsp: bool,
    has_sel: bool,
) -> Vec<MenuItem> {
    // Nothing gutter-specific on this line → reuse the Code menu verbatim.
    if !has_diag && git == GitHunkKind::None {
        return build_code_menu(has_sel, has_lsp);
    }

    let mut items = Vec::new();

    // Diagnostic entries lead when a diagnostic is on the line (#114 P6).
    if has_diag {
        items.push(MenuItem::new(
            "Show Diagnostic",
            MenuAction::DiagnosticDetail,
            Some("<leader>e".into()),
        ));
        items.push(MenuItem {
            label: "Code Actions".into(),
            action: MenuAction::LspCodeActions,
            enabled: has_lsp,
            shortcut_hint: Some("<leader>ca".into()),
        });
        items.push(MenuItem::separator());
    }

    // Git-hunk entries adapt to staged state (#114 P6/P10, #115).
    match git {
        GitHunkKind::Unstaged => {
            items.push(MenuItem::new(
                "Stage Hunk",
                MenuAction::GitStageHunk,
                Some(":GitStage".into()),
            ));
            items.push(MenuItem::new(
                "Revert Hunk",
                MenuAction::GitRevertHunk,
                Some(":GitRevert".into()),
            ));
            items.push(MenuItem::new(
                "Show Hunk Diff",
                MenuAction::GitShowHunk,
                Some(":GitDiff".into()),
            ));
            items.push(MenuItem::separator());
        }
        GitHunkKind::Staged => {
            items.push(MenuItem::new(
                "Unstage Hunk",
                MenuAction::GitUnstageHunk,
                Some(":GitUnstage".into()),
            ));
            items.push(MenuItem::new(
                "Show Hunk Diff",
                MenuAction::GitShowHunk,
                Some(":GitDiff".into()),
            ));
            items.push(MenuItem::separator());
        }
        GitHunkKind::None => {}
    }

    // Append the standard Code-zone actions so navigation / clipboard / format
    // stay reachable from the gutter.
    items.extend(build_code_menu(has_sel, has_lsp));
    items
}

/// Build the context menu for a right-click on the status line.
///
/// `ft` is the file-type label (e.g. `"rust"`, `"(none)"`).
/// `lsp_name` is `Some("rust-analyzer")` when a server is attached, `None`
/// otherwise.
///
/// ```rust
/// use hjkl_menu::{build_status_line_menu, MenuAction};
///
/// let items = build_status_line_menu("rust", Some("rust-analyzer"));
/// assert!(items[0].label.contains("rust"));
/// assert!(!items[0].enabled, "info header must not be selectable");
/// ```
pub fn build_status_line_menu(ft: &str, lsp_name: Option<&str>) -> Vec<MenuItem> {
    let ft_label = format!("Filetype: {ft}");
    let lsp_label = match lsp_name {
        Some(name) => format!("LSP: {name}"),
        None => "LSP: (none)".to_string(),
    };
    let has_lsp = lsp_name.is_some();
    vec![
        // Header: filetype info — not selectable.
        MenuItem {
            label: ft_label,
            action: MenuAction::Info,
            enabled: false,
            shortcut_hint: None,
        },
        // Header: LSP server info — not selectable.
        MenuItem {
            label: lsp_label,
            action: MenuAction::Info,
            enabled: false,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem {
            label: "Restart LSP".into(),
            action: MenuAction::LspRestart,
            enabled: has_lsp,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem::new("Open File…", MenuAction::OpenFilePicker, None),
    ]
}

/// Build the context menu for a right-click on a split border.
///
/// ```rust
/// use hjkl_menu::{build_split_border_menu, MenuAction};
///
/// let items = build_split_border_menu();
/// assert_eq!(items[0].action, MenuAction::WindowEqualize);
/// assert_eq!(items[1].action, MenuAction::WindowClose);
/// ```
pub fn build_split_border_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::new("Equalize Splits", MenuAction::WindowEqualize, None),
        MenuItem::new("Close This Window", MenuAction::WindowClose, None),
    ]
}

/// Build the context menu for a right-click on a picker overlay row.
///
/// `has_path` controls whether the split / tab / copy-path items are enabled.
/// When the focused row has no file-system path (e.g. a git-log entry),
/// those items are shown but disabled.
///
/// ```rust
/// use hjkl_menu::{build_picker_menu, MenuAction};
///
/// let items = build_picker_menu(true);
/// assert!(items.iter().any(|it| it.action == MenuAction::PickerOpen && it.enabled));
/// ```
pub fn build_picker_menu(has_path: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::new("Open", MenuAction::PickerOpen, Some("Enter".into())),
        MenuItem {
            label: "Open in Horizontal Split".into(),
            action: MenuAction::PickerOpenSplit,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Open in Vertical Split".into(),
            action: MenuAction::PickerOpenVSplit,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Open in New Tab".into(),
            action: MenuAction::PickerOpenTab,
            enabled: has_path,
            shortcut_hint: None,
        },
        MenuItem::separator(),
        MenuItem {
            label: "Copy Path".into(),
            action: MenuAction::PickerCopyPath,
            enabled: has_path,
            shortcut_hint: None,
        },
    ]
}

/// Build the context menu for a right-click on the tab bar.
///
/// `more_than_one_tab` disables close actions when there is only one tab open.
///
/// ```rust
/// use hjkl_menu::{build_tab_menu, MenuAction};
///
/// let items = build_tab_menu(false);
/// assert!(!items[0].enabled, "CloseTab disabled when only one tab");
/// let items = build_tab_menu(true);
/// assert!(items[0].enabled);
/// ```
pub fn build_tab_menu(more_than_one_tab: bool) -> Vec<MenuItem> {
    vec![
        MenuItem {
            label: "Close Tab".into(),
            action: MenuAction::TabClose,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Other Tabs".into(),
            action: MenuAction::TabCloseOthers,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Tabs to the Right".into(),
            action: MenuAction::TabCloseRight,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
        MenuItem {
            label: "Close Tabs to the Left".into(),
            action: MenuAction::TabCloseLeft,
            enabled: more_than_one_tab,
            shortcut_hint: None,
        },
    ]
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_menu() -> ContextMenu {
        let items = vec![
            MenuItem::new("Cut", MenuAction::Cut, None),
            MenuItem::new("Copy", MenuAction::Copy, None),
            MenuItem::separator(),
            MenuItem::new("Paste", MenuAction::Paste, None),
        ];
        ContextMenu::new(items, (0, 0))
    }

    // ── MenuAction::is_inert ────────────────────────────────────────────────

    #[test]
    fn separator_is_inert() {
        assert!(MenuAction::Separator.is_inert());
    }

    #[test]
    fn info_is_inert() {
        assert!(MenuAction::Info.is_inert());
    }

    #[test]
    fn copy_is_not_inert() {
        assert!(!MenuAction::Copy.is_inert());
    }

    // ── MenuItem::is_selectable ─────────────────────────────────────────────

    #[test]
    fn enabled_non_inert_is_selectable() {
        let item = MenuItem::new("Copy", MenuAction::Copy, None);
        assert!(item.is_selectable());
    }

    #[test]
    fn disabled_item_is_not_selectable() {
        let item = MenuItem {
            label: "Cut".into(),
            action: MenuAction::Cut,
            enabled: false,
            shortcut_hint: None,
        };
        assert!(!item.is_selectable());
    }

    #[test]
    fn separator_item_is_not_selectable() {
        assert!(!MenuItem::separator().is_selectable());
    }

    // ── ContextMenu::new initial selection ──────────────────────────────────

    #[test]
    fn initial_selected_skips_separator() {
        let items = vec![
            MenuItem::separator(),
            MenuItem::new("Copy", MenuAction::Copy, None),
        ];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn initial_selected_skips_info_header() {
        let items = vec![
            MenuItem {
                label: "Filetype: rust".into(),
                action: MenuAction::Info,
                enabled: false,
                shortcut_hint: None,
            },
            MenuItem::new("Restart LSP", MenuAction::LspRestart, None),
        ];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected, 1);
    }

    #[test]
    fn initial_selected_all_inert_falls_back_to_zero() {
        let items = vec![MenuItem::separator()];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected, 0);
    }

    // ── move_up ─────────────────────────────────────────────────────────────

    #[test]
    fn move_up_from_top_saturates() {
        let mut m = make_menu();
        m.selected = 0;
        m.move_up();
        assert_eq!(m.selected, 0, "should saturate at 0");
    }

    #[test]
    fn move_up_skips_separator() {
        // Items: Cut(0), Copy(1), Sep(2), Paste(3).
        // Start at Paste(3), move_up should skip Sep(2) → Copy(1).
        let mut m = make_menu();
        m.selected = 3;
        m.move_up();
        assert_eq!(m.selected, 1, "should land on Copy skipping separator");
    }

    #[test]
    fn move_up_normal() {
        let mut m = make_menu();
        m.selected = 1; // Copy
        m.move_up();
        assert_eq!(m.selected, 0); // Cut
    }

    // ── move_down ───────────────────────────────────────────────────────────

    #[test]
    fn move_down_skips_separator() {
        // Items: Cut(0), Copy(1), Sep(2), Paste(3).
        // Start at Copy(1), move_down should land on Paste(3).
        let mut m = make_menu();
        m.selected = 1;
        m.move_down();
        assert_eq!(m.selected, 3, "expected Paste at idx 3, got {}", m.selected);
    }

    #[test]
    fn move_down_from_bottom_wraps_to_top() {
        let mut m = make_menu();
        m.selected = 3; // Paste (last selectable)
        m.move_down();
        assert_eq!(m.selected, 0, "should wrap to Cut at idx 0");
    }

    #[test]
    fn move_down_normal() {
        let mut m = make_menu();
        m.selected = 0; // Cut
        m.move_down();
        assert_eq!(m.selected, 1); // Copy
    }

    // ── selected_action ─────────────────────────────────────────────────────

    #[test]
    fn selected_action_copy() {
        let mut m = make_menu();
        m.selected = 1; // Copy
        assert_eq!(m.selected_action(), Some(MenuAction::Copy));
    }

    #[test]
    fn selected_action_separator_is_none() {
        let mut m = make_menu();
        m.selected = 2; // Separator
        assert_eq!(m.selected_action(), None);
    }

    #[test]
    fn selected_action_disabled_is_none() {
        let items = vec![MenuItem {
            label: "Cut".into(),
            action: MenuAction::Cut,
            enabled: false,
            shortcut_hint: None,
        }];
        let m = ContextMenu::new(items, (0, 0));
        assert_eq!(m.selected_action(), None);
    }

    // ── bounding_rect ───────────────────────────────────────────────────────

    #[test]
    fn bounding_rect_anchored_near_bottom_flips_upward() {
        // 24-row screen, anchor at row 22, popup has 6 items + 2 border = 8 rows.
        let items: Vec<MenuItem> = (0..6)
            .map(|i| MenuItem::new(format!("Item {i}"), MenuAction::Paste, None))
            .collect();
        let m = ContextMenu::new(items, (5, 22));
        let (_, y, _, h) = m.bounding_rect(80, 24);

        assert_eq!(h, 8, "popup height = 6 items + 2 border rows; got {h}");
        assert!(
            y + h <= 24,
            "bottom edge ({}+{}={}) must not exceed screen height 24",
            y,
            h,
            y + h,
        );
        assert!(
            y < 22,
            "popup must have shifted up from anchor row 22; got y={y}"
        );
        assert_eq!(y, 24 - 8, "expected popup to sit flush with bottom");
    }

    #[test]
    fn bounding_rect_anchored_near_right_shifts_left() {
        let items = vec![
            MenuItem::new("Reasonably Long Item Label", MenuAction::Paste, None),
            MenuItem::new("Another Long Item Label", MenuAction::Copy, None),
        ];
        let m = ContextMenu::new(items, (75, 5));
        let (x, _, w, _) = m.bounding_rect(80, 24);
        assert!(
            x + w <= 80,
            "right edge {} must not exceed screen width 80; x={x} w={w}",
            x + w,
        );
        assert!(
            x < 75,
            "popup must have shifted left from anchor=75; got x={x}"
        );
    }

    #[test]
    fn bounding_rect_fits_small_anchor() {
        let items = vec![MenuItem::new("Paste", MenuAction::Paste, None)];
        let m = ContextMenu::new(items, (0, 0));
        let (x, y, w, h) = m.bounding_rect(80, 24);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
        assert!(w > 0 && h > 0);
    }

    // ── build_code_menu ─────────────────────────────────────────────────────

    #[test]
    fn build_code_menu_with_selection_enables_cut_copy() {
        let items = build_code_menu(true, false);
        assert!(items[0].enabled); // Cut
        assert!(items[1].enabled); // Copy
        assert!(items[2].enabled); // Paste
    }

    #[test]
    fn build_code_menu_no_selection_disables_cut_copy() {
        let items = build_code_menu(false, false);
        assert!(!items[0].enabled); // Cut
        assert!(!items[1].enabled); // Copy
        assert!(items[2].enabled); // Paste always enabled
    }

    #[test]
    fn build_code_menu_includes_lsp_items_when_lsp_attached() {
        let items = build_code_menu(false, true);
        let lsp_actions = [
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        for action in &lsp_actions {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found in menu"));
            assert!(
                item.enabled,
                "{action:?} should be enabled when has_lsp=true"
            );
        }
    }

    #[test]
    fn build_code_menu_disables_lsp_items_when_no_lsp() {
        let items = build_code_menu(false, false);
        let lsp_actions = [
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        for action in &lsp_actions {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found in menu"));
            assert!(
                !item.enabled,
                "{action:?} should be disabled when has_lsp=false"
            );
        }
    }

    #[test]
    fn build_code_menu_separator_layout() {
        let items = build_code_menu(true, true);
        let expected_order = [
            MenuAction::Cut,
            MenuAction::Copy,
            MenuAction::Paste,
            MenuAction::LspGotoDefinition,
            MenuAction::LspGotoReferences,
            MenuAction::LspHover,
            MenuAction::LspRename,
            MenuAction::LspCodeActions,
            MenuAction::LspFormat,
        ];
        let non_sep: Vec<&MenuAction> = items
            .iter()
            .filter(|it| it.action != MenuAction::Separator)
            .map(|it| &it.action)
            .collect();
        assert_eq!(non_sep.len(), expected_order.len());
        for (got, want) in non_sep.iter().zip(expected_order.iter()) {
            assert_eq!(*got, want, "order mismatch");
        }
        let sep_positions: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.action == MenuAction::Separator)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(sep_positions.len(), 2, "expected exactly 2 separators");
        assert_eq!(items[sep_positions[0]].action, MenuAction::Separator);
        assert_eq!(items[sep_positions[0] - 1].action, MenuAction::Paste);
        assert_eq!(items[sep_positions[1] + 1].action, MenuAction::LspRename);
    }

    // ── build_gutter_menu (#114 P6) ─────────────────────────────────────────

    #[test]
    fn build_gutter_menu_with_diag_leads_with_diagnostic_detail() {
        let items = build_gutter_menu(true, GitHunkKind::None, true, false);
        assert_eq!(
            items[0].action,
            MenuAction::DiagnosticDetail,
            "gutter menu with a diagnostic must lead with Show Diagnostic"
        );
        // Code Actions appears and is enabled when LSP is attached.
        assert!(
            items
                .iter()
                .any(|it| it.action == MenuAction::LspCodeActions && it.enabled),
            "Code Actions must be enabled when LSP is attached"
        );
        // The standard Code menu is still appended below.
        assert!(
            items.iter().any(|it| it.action == MenuAction::Paste),
            "gutter menu must still include the Code actions"
        );
    }

    #[test]
    fn build_gutter_menu_code_actions_disabled_without_lsp() {
        let items = build_gutter_menu(true, GitHunkKind::None, false, false);
        let ca = items
            .iter()
            .find(|it| it.action == MenuAction::LspCodeActions)
            .expect("Code Actions present");
        assert!(!ca.enabled, "Code Actions disabled when no LSP attached");
    }

    #[test]
    fn build_gutter_menu_without_diag_is_plain_code_menu() {
        let gutter = build_gutter_menu(false, GitHunkKind::None, true, false);
        let code = build_code_menu(false, true);
        let g: Vec<&MenuAction> = gutter.iter().map(|it| &it.action).collect();
        let c: Vec<&MenuAction> = code.iter().map(|it| &it.action).collect();
        assert_eq!(g, c, "no diagnostic → identical to the Code menu");
        assert!(
            !gutter
                .iter()
                .any(|it| it.action == MenuAction::DiagnosticDetail),
            "no DiagnosticDetail row when the line has no diagnostic"
        );
    }

    #[test]
    fn build_gutter_menu_unstaged_offers_stage_and_revert() {
        let items = build_gutter_menu(false, GitHunkKind::Unstaged, false, false);
        assert!(items.iter().any(|it| it.action == MenuAction::GitStageHunk));
        assert!(
            items
                .iter()
                .any(|it| it.action == MenuAction::GitRevertHunk)
        );
        assert!(items.iter().any(|it| it.action == MenuAction::GitShowHunk));
        assert!(
            !items
                .iter()
                .any(|it| it.action == MenuAction::GitUnstageHunk),
            "unstaged hunk must not offer Unstage"
        );
    }

    #[test]
    fn build_gutter_menu_staged_offers_unstage_only() {
        let items = build_gutter_menu(false, GitHunkKind::Staged, false, false);
        assert!(
            items
                .iter()
                .any(|it| it.action == MenuAction::GitUnstageHunk)
        );
        assert!(items.iter().any(|it| it.action == MenuAction::GitShowHunk));
        assert!(
            !items.iter().any(|it| it.action == MenuAction::GitStageHunk),
            "staged hunk must not offer Stage"
        );
        assert!(
            !items
                .iter()
                .any(|it| it.action == MenuAction::GitRevertHunk),
            "staged hunk must not offer Revert (use Unstage)"
        );
    }

    #[test]
    fn git_unstage_hunk_dispatches_to_kind() {
        let mut hit = false;
        let handled = MenuAction::GitUnstageHunk.dispatch(|kind| {
            if let MenuActionKind::GitUnstageHunk = kind {
                hit = true;
            }
        });
        assert!(handled && hit, "GitUnstageHunk must dispatch to its kind");
    }

    #[test]
    fn diagnostic_detail_dispatches_to_kind() {
        let mut hit = false;
        let handled = MenuAction::DiagnosticDetail.dispatch(|kind| {
            if let MenuActionKind::DiagnosticDetail = kind {
                hit = true;
            }
        });
        assert!(handled && hit, "DiagnosticDetail must dispatch to its kind");
    }

    // ── build_tab_menu ──────────────────────────────────────────────────────

    #[test]
    fn build_tab_menu_single_tab_disables_close() {
        let items = build_tab_menu(false);
        assert!(!items[0].enabled); // Close Tab
    }

    #[test]
    fn build_tab_menu_multi_tab_enables_close() {
        let items = build_tab_menu(true);
        assert!(items[0].enabled); // Close Tab
        assert!(items[1].enabled); // Close Other Tabs
    }

    // ── build_status_line_menu ──────────────────────────────────────────────

    #[test]
    fn build_status_line_menu_includes_filetype_info() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let ft_item = &items[0];
        assert!(
            ft_item.label.contains("rust"),
            "first item label should contain 'rust', got {:?}",
            ft_item.label
        );
        assert!(
            !ft_item.enabled,
            "filetype info item must not be selectable"
        );
        assert_eq!(
            ft_item.action,
            MenuAction::Info,
            "filetype item uses Info action"
        );
    }

    #[test]
    fn build_status_line_menu_lsp_name_shown() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let lsp_item = &items[1];
        assert!(
            lsp_item.label.contains("rust-analyzer"),
            "lsp item label should contain server name, got {:?}",
            lsp_item.label
        );
    }

    #[test]
    fn build_status_line_menu_restart_disabled_when_no_lsp() {
        let items = build_status_line_menu("(none)", None);
        let restart = items
            .iter()
            .find(|it| it.action == MenuAction::LspRestart)
            .expect("LspRestart item must exist");
        assert!(
            !restart.enabled,
            "LspRestart should be disabled when lsp_name is None"
        );
    }

    #[test]
    fn build_status_line_menu_restart_enabled_when_lsp_present() {
        let items = build_status_line_menu("rust", Some("rust-analyzer"));
        let restart = items
            .iter()
            .find(|it| it.action == MenuAction::LspRestart)
            .expect("LspRestart item must exist");
        assert!(
            restart.enabled,
            "LspRestart should be enabled when lsp_name is Some"
        );
    }

    #[test]
    fn build_status_line_menu_open_file_always_enabled() {
        let items = build_status_line_menu("(none)", None);
        let open = items
            .iter()
            .find(|it| it.action == MenuAction::OpenFilePicker)
            .expect("OpenFilePicker item must exist");
        assert!(open.enabled, "Open File… should always be enabled");
    }

    // ── build_split_border_menu ─────────────────────────────────────────────

    #[test]
    fn build_split_border_menu_has_equalize_and_close() {
        let items = build_split_border_menu();
        let non_sep: Vec<&MenuItem> = items.iter().filter(|it| !it.action.is_inert()).collect();
        assert_eq!(non_sep.len(), 2, "expected exactly 2 real items");
        assert_eq!(non_sep[0].action, MenuAction::WindowEqualize);
        assert_eq!(non_sep[1].action, MenuAction::WindowClose);
        assert!(non_sep[0].enabled);
        assert!(non_sep[1].enabled);
    }

    // ── build_picker_menu ───────────────────────────────────────────────────

    #[test]
    fn build_picker_menu_all_enabled_when_has_path() {
        let items = build_picker_menu(true);
        for it in &items {
            if it.action.is_inert() {
                continue;
            }
            assert!(
                it.enabled,
                "{:?} should be enabled when has_path=true",
                it.action
            );
        }
    }

    #[test]
    fn build_picker_menu_disables_path_items_when_no_path() {
        let items = build_picker_menu(false);
        let open = items
            .iter()
            .find(|it| it.action == MenuAction::PickerOpen)
            .unwrap();
        assert!(open.enabled, "PickerOpen should always be enabled");
        for action in &[
            MenuAction::PickerOpenSplit,
            MenuAction::PickerOpenVSplit,
            MenuAction::PickerOpenTab,
            MenuAction::PickerCopyPath,
        ] {
            let item = items
                .iter()
                .find(|it| &it.action == action)
                .unwrap_or_else(|| panic!("{action:?} not found"));
            assert!(
                !item.enabled,
                "{action:?} should be disabled when has_path=false"
            );
        }
    }
}
