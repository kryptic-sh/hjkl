//! Static descriptor table for the vim FSM's built-in bindings.
//!
//! [`children_for`] returns the direct children of a prefix in the engine
//! FSM's dispatch table for a given [`crate::Mode`]. The caller (typically
//! `which_key::entries_for` in the hjkl app) merges these with app-keymap
//! entries; app bindings win on conflict (`:nmap` shadows builtins).
//!
//! **Completeness policy (v1):** Normal-mode root, g-prefix, z-prefix, and
//! operator-pending (d/c/y) children are covered. Visual-mode and text-object
//! completeness are out of scope for v1 per issue #64.
//!
//! **Drift risk:** The table is hand-maintained. If `normal.rs` or
//! `hjkl-engine`'s `apply_after_g` / `apply_after_z` add a new binding,
//! this table will miss it until updated. The `COUNT_*` constants + tests
//! assert exact counts so drift triggers test failures, not silent gaps.

use hjkl_keymap::{KeyCode, KeyEvent, KeyModifiers};

use crate::Mode;

// ─── Public types ─────────────────────────────────────────────────────────────

/// A single built-in vim FSM binding, for which-key display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimDescriptor {
    /// The key event that triggers this binding.
    pub key: KeyEvent,
    /// Short human-readable description.
    /// `None` means prefix-only node (submenu — shows as "…" in the popup).
    pub desc: Option<&'static str>,
}

impl VimDescriptor {
    fn char(c: char, desc: &'static str) -> Self {
        Self {
            key: KeyEvent::char(c),
            desc: Some(desc),
        }
    }

    fn ctrl(c: char, desc: &'static str) -> Self {
        Self {
            key: KeyEvent::ctrl(c),
            desc: Some(desc),
        }
    }

    fn prefix(c: char) -> Self {
        Self {
            key: KeyEvent::char(c),
            desc: None,
        }
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Return the direct children of `prefix` in the engine FSM's dispatch table
/// for the given `mode`.
///
/// - Empty prefix → root keys for the mode.
/// - `[KeyEvent::char('g')]` → g-prefix children.
/// - `[KeyEvent::char('z')]` → z-prefix children.
/// - `[KeyEvent::char('d')]` / `['c']` / `['y']` → operator-pending children
///   (motions + sub-prefixes for text objects).
/// - Unknown prefix → empty.
///
/// Results are in declaration order (not sorted — the caller sorts for display).
pub fn children_for(mode: Mode, prefix: &[KeyEvent]) -> Vec<VimDescriptor> {
    match mode {
        Mode::Normal => children_normal(prefix),
        Mode::Visual | Mode::VisualLine | Mode::VisualBlock => children_visual(prefix),
        Mode::OpPending => children_op_pending(prefix),
        Mode::Insert | Mode::CommandLine => vec![],
    }
}

// ─── Expected counts (used by tests to catch drift) ───────────────────────────

/// Expected count of root Normal-mode descriptors.
pub const COUNT_NORMAL_ROOT: usize = 83;
/// Expected count of g-prefix descriptors.
pub const COUNT_G_PREFIX: usize = 19;
/// Expected count of z-prefix descriptors.
pub const COUNT_Z_PREFIX: usize = 11;
/// Expected count of operator-pending root descriptors (d/c/y prefix children).
pub const COUNT_OP_PENDING_ROOT: usize = 24;

// ─── Normal-mode dispatch ──────────────────────────────────────────────────────

fn children_normal(prefix: &[KeyEvent]) -> Vec<VimDescriptor> {
    if prefix.is_empty() {
        return normal_root();
    }
    // Single-char prefixes.
    if prefix.len() == 1 {
        let k = prefix[0];
        if k == KeyEvent::char('g') {
            return g_prefix();
        }
        if k == KeyEvent::char('z') {
            return z_prefix();
        }
        // Operator prefixes — show motion/text-object children.
        if k == KeyEvent::char('d')
            || k == KeyEvent::char('c')
            || k == KeyEvent::char('y')
            || k == KeyEvent::char('>')
            || k == KeyEvent::char('<')
            || k == KeyEvent::char('=')
        {
            return op_pending_root();
        }
    }
    vec![]
}

fn children_visual(prefix: &[KeyEvent]) -> Vec<VimDescriptor> {
    if prefix.is_empty() {
        return visual_root();
    }
    if prefix.len() == 1 && prefix[0] == KeyEvent::char('z') {
        return z_prefix();
    }
    vec![]
}

fn children_op_pending(prefix: &[KeyEvent]) -> Vec<VimDescriptor> {
    if prefix.is_empty() {
        return op_pending_root();
    }
    vec![]
}

// ─── Root tables ──────────────────────────────────────────────────────────────

fn normal_root() -> Vec<VimDescriptor> {
    // Sourced from normal.rs: handle_normal_only + parse_motion + pending-entry
    // arms + Ctrl branches. Keys the engine silently swallows but doesn't act
    // on (unknown chars) are excluded.
    vec![
        // ── Insert-mode entries ───────────────────────────────────────────────
        VimDescriptor::char('i', "insert before cursor"),
        VimDescriptor::char('I', "insert at line start"),
        VimDescriptor::char('a', "append after cursor"),
        VimDescriptor::char('A', "append at line end"),
        VimDescriptor::char('o', "open line below"),
        VimDescriptor::char('O', "open line above"),
        VimDescriptor::char('R', "enter replace mode"),
        VimDescriptor::char('s', "substitute char"),
        VimDescriptor::char('S', "substitute line"),
        // ── Delete / change / yank ────────────────────────────────────────────
        VimDescriptor::prefix('d'),
        VimDescriptor::prefix('c'),
        VimDescriptor::prefix('y'),
        VimDescriptor::char('x', "delete char forward"),
        VimDescriptor::char('X', "delete char backward"),
        VimDescriptor::char('D', "delete to end of line"),
        VimDescriptor::char('C', "change to end of line"),
        VimDescriptor::char('Y', "yank to end of line"),
        // ── Paste / undo / redo ───────────────────────────────────────────────
        VimDescriptor::char('p', "paste after"),
        VimDescriptor::char('P', "paste before"),
        VimDescriptor::char('u', "undo"),
        VimDescriptor::ctrl('r', "redo"),
        // ── Case / misc edits ─────────────────────────────────────────────────
        VimDescriptor::char('~', "toggle case at cursor"),
        VimDescriptor::char('J', "join line below"),
        VimDescriptor::char('r', "replace character"),
        VimDescriptor::char('.', "repeat last change"),
        // ── Indentation ───────────────────────────────────────────────────────
        VimDescriptor::prefix('>'),
        VimDescriptor::prefix('<'),
        VimDescriptor::prefix('='),
        // ── Motions ───────────────────────────────────────────────────────────
        VimDescriptor::char('h', "left"),
        VimDescriptor::char('j', "down"),
        VimDescriptor::char('k', "up"),
        VimDescriptor::char('l', "right"),
        VimDescriptor::char('w', "word forward"),
        VimDescriptor::char('W', "WORD forward"),
        VimDescriptor::char('b', "word backward"),
        VimDescriptor::char('B', "WORD backward"),
        VimDescriptor::char('e', "word end"),
        VimDescriptor::char('E', "WORD end"),
        VimDescriptor::char('0', "line start"),
        VimDescriptor::char('^', "first non-blank"),
        VimDescriptor::char('$', "line end"),
        VimDescriptor::char('G', "file bottom / go to line"),
        VimDescriptor::char('%', "match bracket"),
        VimDescriptor::char('H', "viewport top"),
        VimDescriptor::char('M', "viewport middle"),
        VimDescriptor::char('L', "viewport bottom"),
        VimDescriptor::char('{', "paragraph prev"),
        VimDescriptor::char('}', "paragraph next"),
        VimDescriptor::char('(', "sentence prev"),
        VimDescriptor::char(')', "sentence next"),
        VimDescriptor::char('n', "search next"),
        VimDescriptor::char('N', "search prev"),
        VimDescriptor::char('*', "search word forward"),
        VimDescriptor::char('#', "search word backward"),
        VimDescriptor::char(';', "repeat find"),
        VimDescriptor::char(',', "repeat find reverse"),
        // ── Find char ────────────────────────────────────────────────────────
        VimDescriptor::char('f', "find char forward"),
        VimDescriptor::char('F', "find char backward"),
        VimDescriptor::char('t', "till char forward"),
        VimDescriptor::char('T', "till char backward"),
        // ── Prefix keys ───────────────────────────────────────────────────────
        VimDescriptor::prefix('g'),
        VimDescriptor::prefix('z'),
        // ── Marks ────────────────────────────────────────────────────────────
        VimDescriptor::char('m', "set mark"),
        VimDescriptor::char('\'', "goto mark (linewise)"),
        VimDescriptor::char('`', "goto mark (charwise)"),
        // ── Registers / macros ────────────────────────────────────────────────
        VimDescriptor::char('"', "select register"),
        VimDescriptor::char('@', "play macro"),
        VimDescriptor::char('q', "record macro"),
        // ── Scroll ───────────────────────────────────────────────────────────
        VimDescriptor::ctrl('d', "scroll half-page down"),
        VimDescriptor::ctrl('u', "scroll half-page up"),
        VimDescriptor::ctrl('f', "scroll full-page down"),
        VimDescriptor::ctrl('b', "scroll full-page up"),
        VimDescriptor::ctrl('e', "scroll line down"),
        VimDescriptor::ctrl('y', "scroll line up"),
        // ── Number adjust ────────────────────────────────────────────────────
        VimDescriptor::ctrl('a', "increment number"),
        VimDescriptor::ctrl('x', "decrement number"),
        // ── Jump list ────────────────────────────────────────────────────────
        VimDescriptor::ctrl('o', "jump back"),
        VimDescriptor::ctrl('i', "jump forward"),
        // ── Visual mode entry ────────────────────────────────────────────────
        VimDescriptor::char('v', "enter visual mode"),
        VimDescriptor::char('V', "enter visual-line mode"),
        VimDescriptor {
            key: KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CTRL),
            desc: Some("enter visual-block mode"),
        },
        // ── Search ───────────────────────────────────────────────────────────
        VimDescriptor::char('/', "search forward"),
        VimDescriptor::char('?', "search backward"),
    ]
}

fn visual_root() -> Vec<VimDescriptor> {
    vec![
        // Motions (same as normal) — subset most useful in visual.
        VimDescriptor::char('h', "left"),
        VimDescriptor::char('j', "down"),
        VimDescriptor::char('k', "up"),
        VimDescriptor::char('l', "right"),
        VimDescriptor::char('w', "word forward"),
        VimDescriptor::char('b', "word backward"),
        VimDescriptor::char('e', "word end"),
        VimDescriptor::char('0', "line start"),
        VimDescriptor::char('$', "line end"),
        VimDescriptor::char('G', "file bottom"),
        VimDescriptor::char('%', "match bracket"),
        VimDescriptor::char('n', "search next"),
        VimDescriptor::char('N', "search prev"),
        // Visual operators.
        VimDescriptor::char('d', "delete selection"),
        VimDescriptor::char('c', "change selection"),
        VimDescriptor::char('y', "yank selection"),
        VimDescriptor::char('x', "delete selection"),
        VimDescriptor::char('s', "substitute selection"),
        VimDescriptor::char('U', "uppercase selection"),
        VimDescriptor::char('u', "lowercase selection"),
        VimDescriptor::char('~', "toggle case selection"),
        VimDescriptor::char('>', "indent selection"),
        VimDescriptor::char('<', "outdent selection"),
        VimDescriptor::char('=', "auto-indent selection"),
        VimDescriptor::char('o', "swap anchor and cursor"),
        // Text-object prefix.
        VimDescriptor::char('i', "inner text object"),
        VimDescriptor::char('a', "around text object"),
        // Prefix.
        VimDescriptor::prefix('z'),
        // Mark goto.
        VimDescriptor::char('`', "goto mark (charwise)"),
    ]
}

fn g_prefix() -> Vec<VimDescriptor> {
    // Sourced from apply_after_g in hjkl-engine::vim (confirmed against actual dispatch).
    vec![
        VimDescriptor::char('g', "go to first line"),
        VimDescriptor::char('e', "word end backward"),
        VimDescriptor::char('E', "WORD end backward"),
        VimDescriptor::char('_', "last non-blank on line"),
        VimDescriptor::char('M', "middle of line"),
        VimDescriptor::char('v', "reselect last visual"),
        VimDescriptor::char('j', "display-line down"),
        VimDescriptor::char('k', "display-line up"),
        VimDescriptor::char('U', "uppercase operator"),
        VimDescriptor::char('u', "lowercase operator"),
        VimDescriptor::char('~', "toggle case operator"),
        VimDescriptor::char('q', "reflow operator"),
        VimDescriptor::char('J', "join without space"),
        VimDescriptor::char('d', "goto definition"),
        VimDescriptor::char('i', "goto last insert position"),
        VimDescriptor::char(';', "goto older change"),
        VimDescriptor::char(',', "goto newer change"),
        VimDescriptor::char('*', "search word (partial) forward"),
        VimDescriptor::char('#', "search word (partial) backward"),
    ]
}

fn z_prefix() -> Vec<VimDescriptor> {
    // Sourced from apply_after_z in hjkl-engine::vim (confirmed against actual dispatch).
    vec![
        VimDescriptor::char('z', "center cursor line"),
        VimDescriptor::char('t', "cursor line to top"),
        VimDescriptor::char('b', "cursor line to bottom"),
        VimDescriptor::char('o', "open fold"),
        VimDescriptor::char('c', "close fold"),
        VimDescriptor::char('a', "toggle fold"),
        VimDescriptor::char('R', "open all folds"),
        VimDescriptor::char('M', "close all folds"),
        VimDescriptor::char('E', "clear all folds"),
        VimDescriptor::char('d', "delete fold at cursor"),
        VimDescriptor::char('f', "create fold (visual/motion)"),
    ]
}

fn op_pending_root() -> Vec<VimDescriptor> {
    // Motion keys available after d/c/y (same parse_motion table as normal mode).
    // Also includes text-object prefixes (i/a) and g sub-prefix.
    vec![
        VimDescriptor::char('h', "left"),
        VimDescriptor::char('j', "down"),
        VimDescriptor::char('k', "up"),
        VimDescriptor::char('l', "right"),
        VimDescriptor::char('w', "word forward"),
        VimDescriptor::char('W', "WORD forward"),
        VimDescriptor::char('b', "word backward"),
        VimDescriptor::char('B', "WORD backward"),
        VimDescriptor::char('e', "word end"),
        VimDescriptor::char('E', "WORD end"),
        VimDescriptor::char('0', "line start"),
        VimDescriptor::char('^', "first non-blank"),
        VimDescriptor::char('$', "line end"),
        VimDescriptor::char('G', "file bottom"),
        VimDescriptor::char('%', "match bracket"),
        VimDescriptor::char('n', "search next"),
        VimDescriptor::char('N', "search prev"),
        VimDescriptor::char('f', "find char forward"),
        VimDescriptor::char('F', "find char backward"),
        VimDescriptor::char('t', "till char forward"),
        VimDescriptor::char('T', "till char backward"),
        // Text-object prefixes.
        VimDescriptor::char('i', "inner text object"),
        VimDescriptor::char('a', "around text object"),
        // g sub-prefix (dgg / dge etc.).
        VimDescriptor::prefix('g'),
    ]
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Normal root ──────────────────────────────────────────────────────────

    #[test]
    fn normal_root_count_matches_expected() {
        let got = children_for(Mode::Normal, &[]);
        assert_eq!(
            got.len(),
            COUNT_NORMAL_ROOT,
            "normal root count drifted: got {}, expected {}",
            got.len(),
            COUNT_NORMAL_ROOT
        );
    }

    #[test]
    fn normal_root_includes_basic_motions() {
        let got = children_for(Mode::Normal, &[]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        for ch in ['h', 'j', 'k', 'l'] {
            assert!(
                keys.contains(&KeyEvent::char(ch)),
                "normal root missing '{ch}'"
            );
        }
    }

    #[test]
    fn normal_root_includes_insert_entries() {
        let got = children_for(Mode::Normal, &[]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        for ch in ['i', 'a', 'I', 'A', 'o', 'O'] {
            assert!(
                keys.contains(&KeyEvent::char(ch)),
                "normal root missing insert entry '{ch}'"
            );
        }
    }

    #[test]
    fn normal_root_g_and_z_are_prefix_nodes() {
        let got = children_for(Mode::Normal, &[]);
        let g = got.iter().find(|d| d.key == KeyEvent::char('g')).unwrap();
        let z = got.iter().find(|d| d.key == KeyEvent::char('z')).unwrap();
        assert_eq!(g.desc, None, "g should be a prefix node (desc = None)");
        assert_eq!(z.desc, None, "z should be a prefix node (desc = None)");
    }

    #[test]
    fn normal_root_operator_prefixes_are_prefix_nodes() {
        let got = children_for(Mode::Normal, &[]);
        for ch in ['d', 'c', 'y'] {
            let entry = got
                .iter()
                .find(|d| d.key == KeyEvent::char(ch))
                .unwrap_or_else(|| panic!("normal root missing operator prefix '{ch}'"));
            assert_eq!(
                entry.desc, None,
                "operator '{ch}' should be a prefix node (desc = None)"
            );
        }
    }

    #[test]
    fn normal_root_has_ctrl_scroll_keys() {
        let got = children_for(Mode::Normal, &[]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        for ch in ['d', 'u', 'f', 'b', 'e', 'y'] {
            assert!(
                keys.contains(&KeyEvent::ctrl(ch)),
                "normal root missing <C-{ch}>"
            );
        }
    }

    // ── g-prefix ────────────────────────────────────────────────────────────

    #[test]
    fn g_prefix_count_matches_expected() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('g')]);
        assert_eq!(
            got.len(),
            COUNT_G_PREFIX,
            "g-prefix count drifted: got {}, expected {}",
            got.len(),
            COUNT_G_PREFIX
        );
    }

    #[test]
    fn g_prefix_includes_gg() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('g')]);
        let found = got
            .iter()
            .any(|d| d.key == KeyEvent::char('g') && d.desc.is_some());
        assert!(found, "g-prefix missing gg");
    }

    #[test]
    fn g_prefix_includes_gj_gk() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('g')]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        assert!(keys.contains(&KeyEvent::char('j')), "g-prefix missing gj");
        assert!(keys.contains(&KeyEvent::char('k')), "g-prefix missing gk");
    }

    #[test]
    fn g_prefix_includes_gd() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('g')]);
        let found = got.iter().any(|d| d.key == KeyEvent::char('d'));
        assert!(found, "g-prefix missing gd (goto definition)");
    }

    #[test]
    fn g_prefix_includes_case_operators() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('g')]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        for ch in ['U', 'u', '~', 'q'] {
            assert!(keys.contains(&KeyEvent::char(ch)), "g-prefix missing g{ch}");
        }
    }

    // ── z-prefix ────────────────────────────────────────────────────────────

    #[test]
    fn z_prefix_count_matches_expected() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('z')]);
        assert_eq!(
            got.len(),
            COUNT_Z_PREFIX,
            "z-prefix count drifted: got {}, expected {}",
            got.len(),
            COUNT_Z_PREFIX
        );
    }

    #[test]
    fn z_prefix_includes_zz() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('z')]);
        let found = got
            .iter()
            .any(|d| d.key == KeyEvent::char('z') && d.desc.is_some());
        assert!(found, "z-prefix missing zz");
    }

    #[test]
    fn z_prefix_includes_zt_zb() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('z')]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        assert!(keys.contains(&KeyEvent::char('t')), "z-prefix missing zt");
        assert!(keys.contains(&KeyEvent::char('b')), "z-prefix missing zb");
    }

    #[test]
    fn z_prefix_includes_fold_ops() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('z')]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        for ch in ['o', 'c', 'a', 'R', 'M', 'E', 'd', 'f'] {
            assert!(keys.contains(&KeyEvent::char(ch)), "z-prefix missing z{ch}");
        }
    }

    // ── operator-pending prefix ──────────────────────────────────────────────

    #[test]
    fn op_pending_root_count_matches_expected() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('d')]);
        assert_eq!(
            got.len(),
            COUNT_OP_PENDING_ROOT,
            "op-pending root count drifted: got {}, expected {}",
            got.len(),
            COUNT_OP_PENDING_ROOT
        );
    }

    #[test]
    fn op_pending_same_for_d_c_y() {
        let d = children_for(Mode::Normal, &[KeyEvent::char('d')]);
        let c = children_for(Mode::Normal, &[KeyEvent::char('c')]);
        let y = children_for(Mode::Normal, &[KeyEvent::char('y')]);
        assert_eq!(d, c, "d and c op-pending children should match");
        assert_eq!(d, y, "d and y op-pending children should match");
    }

    #[test]
    fn op_pending_has_text_object_prefixes() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('d')]);
        let keys: Vec<_> = got.iter().map(|d| d.key).collect();
        assert!(
            keys.contains(&KeyEvent::char('i')),
            "op-pending missing 'i' (inner text obj)"
        );
        assert!(
            keys.contains(&KeyEvent::char('a')),
            "op-pending missing 'a' (around text obj)"
        );
    }

    #[test]
    fn op_pending_has_g_sub_prefix() {
        let got = children_for(Mode::Normal, &[KeyEvent::char('d')]);
        let g = got
            .iter()
            .find(|d| d.key == KeyEvent::char('g'))
            .expect("op-pending missing g sub-prefix");
        assert_eq!(g.desc, None, "g in op-pending should be a prefix node");
    }

    // ── Unknown prefix → empty ────────────────────────────────────────────────

    #[test]
    fn unknown_prefix_returns_empty() {
        // 'q' has no sub-prefix in the engine (it opens RecordMacroTarget,
        // which is a one-key pending, not a named prefix).
        let got = children_for(Mode::Normal, &[KeyEvent::char('q')]);
        assert!(got.is_empty(), "unknown prefix should return empty vec");
    }

    #[test]
    fn insert_mode_always_empty() {
        assert!(children_for(Mode::Insert, &[]).is_empty());
        assert!(children_for(Mode::Insert, &[KeyEvent::char('g')]).is_empty());
    }

    #[test]
    fn op_pending_mode_root_matches_normal_d_prefix() {
        let via_normal = children_for(Mode::Normal, &[KeyEvent::char('d')]);
        let via_op = children_for(Mode::OpPending, &[]);
        assert_eq!(via_normal, via_op);
    }

    // ── Visual mode ──────────────────────────────────────────────────────────

    #[test]
    fn visual_mode_root_non_empty() {
        let got = children_for(Mode::Visual, &[]);
        assert!(!got.is_empty(), "visual root should not be empty");
    }

    #[test]
    fn visual_mode_z_prefix_same_as_normal() {
        let vn = children_for(Mode::Visual, &[KeyEvent::char('z')]);
        let nn = children_for(Mode::Normal, &[KeyEvent::char('z')]);
        assert_eq!(vn, nn, "visual z-prefix should equal normal z-prefix");
    }
}
