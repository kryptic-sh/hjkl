//! Vim-style register bank.
//!
//! Slots:
//! - `"` (unnamed) — written by every `y` / `d` / `c` / `x`; the
//!   default source for `p` / `P`.
//! - `"0` — the most recent **yank** (only when no register was
//!   named). Deletes do not touch it, so `yw…dw…p` still pastes the
//!   original yank.
//! - `"1`–`"9` — numbered delete/change ring. A delete of a whole
//!   line or spanning more than one line shifts the ring (newest at
//!   `"1`, oldest dropped off `"9`). Deletes of less than one line
//!   do **not** touch the ring — they go to `"-` instead.
//! - `"-` — small-delete register: text from a delete/change of less
//!   than one line (unless the command named another register).
//! - `"a`–`"z` — named slots. A capital letter (`"A`…) appends to
//!   the matching lowercase slot, matching vim semantics.

#[derive(Default, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Slot {
    pub text: String,
    pub linewise: bool,
    /// Blockwise (visual-block `<C-v>`) register. When set, `text` still
    /// holds the row segments joined by `\n` (so charwise-fallback paste
    /// and the `hjkl_get_register` RPC keep the same string), but `p`/`P`
    /// re-insert it as COLUMNS at the cursor rather than spilling onto new
    /// lines. Mutually exclusive with `linewise` in practice.
    ///
    /// Additive field: `#[serde(default)]` keeps old serialized register
    /// banks deserializable, and the embed `hjkl_get_register` handler
    /// serialises only `text` + `linewise`, so the wire shape is unchanged.
    #[cfg_attr(feature = "serde", serde(default))]
    pub blockwise: bool,
    /// Column width of a blockwise register — the number of cells each row
    /// segment is padded to (with trailing spaces) when pasted. Zero for
    /// charwise/linewise slots. See [`Slot::blockwise`].
    #[cfg_attr(feature = "serde", serde(default))]
    pub block_width: usize,
}

impl Slot {
    fn new(text: String, linewise: bool) -> Self {
        Self {
            text,
            linewise,
            blockwise: false,
            block_width: 0,
        }
    }

    /// Build a blockwise slot (see [`Slot::blockwise`]). `width` is the
    /// column width every row segment pads to on paste.
    fn new_block(text: String, width: usize) -> Self {
        Self {
            text,
            linewise: false,
            blockwise: true,
            block_width: width,
        }
    }
}

#[derive(Default, Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Registers {
    /// `"` — written by every yank / delete / change.
    pub unnamed: Slot,
    /// `"0` — last yank only.
    pub yank_zero: Slot,
    /// `"1`–`"9` — last 9 line-sized deletes (`"1` newest).
    pub delete_ring: [Slot; 9],
    /// `"-` — small-delete register: last delete/change of less than
    /// one line, when no register was named.
    pub small_delete: Slot,
    /// `"a`–`"z` — named user registers.
    pub named: [Slot; 26],
    /// `"+` / `"*` — system clipboard register. Both selectors alias
    /// the same slot (matches the typical Linux/macOS/Windows setup
    /// where there's no separate primary selection in our pipeline).
    /// The host (the host) syncs this slot from the OS clipboard
    /// before paste and from the slot back out on yank.
    pub clip: Slot,
    /// `"%` — synthetic read-only register: current buffer filename.
    /// Set by the host whenever the active slot changes.
    pub filename: Option<String>,
    /// Pre-built `Slot` for the `%` register. Kept in sync with `filename`
    /// by [`Registers::set_filename`] so `read('%')` can return `&Slot`.
    /// Derived from `filename` — not serialised independently.
    #[cfg_attr(feature = "serde", serde(skip))]
    filename_slot: Option<Slot>,
}

impl Registers {
    /// Record a yank operation. Writes to `"`, `"0`, and (if
    /// `target` is set) the named slot. When `target` is `'_'`
    /// (black-hole register) all writes are suppressed — vim discards
    /// the text without touching any register.
    pub fn record_yank(&mut self, text: String, linewise: bool, target: Option<char>) {
        // Black-hole register: discard the text entirely.
        if target == Some('_') {
            return;
        }
        self.store_yank(Slot::new(text, linewise), target);
    }

    /// Record a blockwise (visual-block) yank. Same routing as
    /// [`Registers::record_yank`] but the resulting slot is flagged
    /// blockwise with column `width` so `p`/`P` re-insert the segments as
    /// columns (see [`Slot::blockwise`]).
    pub fn record_yank_block(&mut self, text: String, width: usize, target: Option<char>) {
        if target == Some('_') {
            return;
        }
        self.store_yank(Slot::new_block(text, width), target);
    }

    /// Shared routing for charwise/linewise/blockwise yanks. Writes `"`,
    /// `"0` (only when unnamed), and the named target if any.
    fn store_yank(&mut self, slot: Slot, target: Option<char>) {
        self.unnamed = slot.clone();
        // vim: `"0` holds the last yank only when the command did not name
        // another register — `"ayy` leaves `"0` untouched.
        if target.is_none() {
            self.yank_zero = slot.clone();
        }
        if let Some(c) = target {
            self.write_named(c, slot);
            // vim: the unnamed register points at the named register just
            // written, so an uppercase-append (`"Ayy`) leaves `"` holding the
            // full appended contents, not just the latest fragment.
            if let Some(named) = self.read(c) {
                self.unnamed = named.clone();
            }
        }
    }

    /// Record a delete / change. Writes to `"` and routes the text to a
    /// numbered or small-delete register (and, if `target` is set, the
    /// named slot). Empty deletes are dropped — vim doesn't pollute the
    /// ring with no-ops. When `target` is `'_'` (black-hole register) all
    /// writes are suppressed, preserving the previous register state.
    ///
    /// Register routing follows vim (`:help quote1`, `:help quote_-`):
    /// - a named target suppresses both the numbered ring and `"-`;
    /// - otherwise a delete of a whole line or spanning more than one line
    ///   shifts the `"1`–`"9` ring;
    /// - a smaller (sub-line) delete goes to `"-` and leaves the ring alone.
    pub fn record_delete(&mut self, text: String, linewise: bool, target: Option<char>) {
        if text.is_empty() {
            return;
        }
        // Black-hole register: discard the text entirely.
        if target == Some('_') {
            return;
        }
        self.store_delete(Slot::new(text, linewise), target);
    }

    /// Record a blockwise (visual-block) delete / change. Same routing as
    /// [`Registers::record_delete`] but flags the slot blockwise with
    /// column `width` (see [`Slot::blockwise`]).
    pub fn record_delete_block(&mut self, text: String, width: usize, target: Option<char>) {
        if text.is_empty() {
            return;
        }
        if target == Some('_') {
            return;
        }
        self.store_delete(Slot::new_block(text, width), target);
    }

    /// Shared routing for charwise/linewise/blockwise deletes/changes.
    fn store_delete(&mut self, slot: Slot, target: Option<char>) {
        self.unnamed = slot.clone();
        if let Some(c) = target {
            // A named register suppresses the numbered ring and `"-`.
            self.write_named(c, slot);
            // vim: unnamed points at the named register just written.
            if let Some(named) = self.read(c) {
                self.unnamed = named.clone();
            }
            return;
        }
        if slot.linewise || slot.text.contains('\n') {
            // Line-sized delete: shift the numbered ring, newest into `"1`.
            for i in (1..9).rev() {
                self.delete_ring[i] = self.delete_ring[i - 1].clone();
            }
            self.delete_ring[0] = slot;
        } else {
            // Small delete: goes to `"-`, ring untouched.
            self.small_delete = slot;
        }
    }

    /// Read a register by its single-char selector. Returns `None`
    /// for unrecognised selectors.
    ///
    /// `'%'` is a synthetic read-only register: returns the current buffer
    /// filename when one has been set via [`Registers::set_filename`].
    pub fn read(&self, reg: char) -> Option<&Slot> {
        match reg {
            '"' => Some(&self.unnamed),
            '0' => Some(&self.yank_zero),
            '1'..='9' => Some(&self.delete_ring[(reg as u8 - b'1') as usize]),
            '-' => Some(&self.small_delete),
            'a'..='z' => Some(&self.named[(reg as u8 - b'a') as usize]),
            'A'..='Z' => Some(&self.named[(reg.to_ascii_lowercase() as u8 - b'a') as usize]),
            '+' | '*' => Some(&self.clip),
            // `%` is a synthetic read-only register: current buffer filename.
            '%' => self.filename_slot.as_ref(),
            _ => None,
        }
    }

    /// Host hook: set the `"%` register to the given filename. Call this
    /// whenever the active buffer changes.
    pub fn set_filename(&mut self, name: Option<String>) {
        self.filename = name.clone();
        self.filename_slot = name.map(|n| Slot::new(n, false));
    }

    /// Replace the clipboard slot's contents — host hook for syncing
    /// from the OS clipboard before a paste from `"+` / `"*`.
    pub fn set_clipboard(&mut self, text: String, linewise: bool) {
        self.clip = Slot::new(text, linewise);
    }

    fn write_named(&mut self, c: char, slot: Slot) {
        if c.is_ascii_lowercase() {
            self.named[(c as u8 - b'a') as usize] = slot;
        } else if c.is_ascii_uppercase() {
            let idx = (c.to_ascii_lowercase() as u8 - b'a') as usize;
            let cur = &mut self.named[idx];
            cur.text.push_str(&slot.text);
            cur.linewise = slot.linewise || cur.linewise;
            cur.blockwise = slot.blockwise || cur.blockwise;
            cur.block_width = cur.block_width.max(slot.block_width);
        } else if c == '+' || c == '*' {
            self.clip = slot;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yank_writes_unnamed_and_zero() {
        let mut r = Registers::default();
        r.record_yank("foo".into(), false, None);
        assert_eq!(r.read('"').unwrap().text, "foo");
        assert_eq!(r.read('0').unwrap().text, "foo");
    }

    #[test]
    fn delete_rotates_ring_and_skips_zero() {
        let mut r = Registers::default();
        r.record_yank("kept".into(), false, None);
        // Line-sized deletes fill the numbered ring.
        r.record_delete("d1\n".into(), true, None);
        r.record_delete("d2\n".into(), true, None);
        // Newest delete is "1.
        assert_eq!(r.read('1').unwrap().text, "d2\n");
        assert_eq!(r.read('2').unwrap().text, "d1\n");
        // "0 untouched by deletes.
        assert_eq!(r.read('0').unwrap().text, "kept");
        // Unnamed mirrors the latest write.
        assert_eq!(r.read('"').unwrap().text, "d2\n");
    }

    #[test]
    fn small_delete_goes_to_dash_not_ring() {
        let mut r = Registers::default();
        // Seed the ring with a line-sized delete.
        r.record_delete("line\n".into(), true, None);
        // Sub-line deletes (e.g. `x`, `dw`) land in "- and leave the ring.
        r.record_delete("x".into(), false, None);
        r.record_delete("y".into(), false, None);
        assert_eq!(r.read('-').unwrap().text, "y");
        // Ring still holds the earlier line delete, unshifted.
        assert_eq!(r.read('1').unwrap().text, "line\n");
        assert!(r.read('2').unwrap().text.is_empty());
        // Unnamed still mirrors the latest write.
        assert_eq!(r.read('"').unwrap().text, "y");
    }

    #[test]
    fn multiline_charwise_delete_fills_ring_not_dash() {
        let mut r = Registers::default();
        // A charwise delete spanning a newline is "line-sized" for routing.
        r.record_delete("a\nb".into(), false, None);
        assert_eq!(r.read('1').unwrap().text, "a\nb");
        assert!(r.read('-').unwrap().text.is_empty());
    }

    #[test]
    fn named_delete_leaves_ring_and_dash_untouched() {
        let mut r = Registers::default();
        r.record_delete("ring\n".into(), true, None);
        r.record_delete("dash".into(), false, None);
        // `"add` — named target suppresses both ring and "-.
        r.record_delete("named\n".into(), true, Some('a'));
        assert_eq!(r.read('a').unwrap().text, "named\n");
        assert_eq!(r.read('1').unwrap().text, "ring\n");
        assert_eq!(r.read('-').unwrap().text, "dash");
        // Unnamed mirrors the named write.
        assert_eq!(r.read('"').unwrap().text, "named\n");
    }

    #[test]
    fn yank_to_named_preserves_zero() {
        let mut r = Registers::default();
        r.record_yank("original".into(), false, None);
        assert_eq!(r.read('0').unwrap().text, "original");
        // `"ayy` must not clobber "0.
        r.record_yank("into a".into(), false, Some('a'));
        assert_eq!(r.read('0').unwrap().text, "original");
        assert_eq!(r.read('a').unwrap().text, "into a");
    }

    #[test]
    fn named_lowercase_overwrites_uppercase_appends() {
        let mut r = Registers::default();
        r.record_yank("hello ".into(), false, Some('a'));
        r.record_yank("world".into(), false, Some('A'));
        assert_eq!(r.read('a').unwrap().text, "hello world");
        // "A is just a write target; reading 'A' returns the same slot.
        assert_eq!(r.read('A').unwrap().text, "hello world");
    }

    #[test]
    fn empty_delete_is_dropped() {
        let mut r = Registers::default();
        r.record_delete("first\n".into(), true, None);
        r.record_delete(String::new(), true, None);
        assert_eq!(r.read('1').unwrap().text, "first\n");
        assert!(r.read('2').unwrap().text.is_empty());
    }

    #[test]
    fn unknown_selector_returns_none() {
        let r = Registers::default();
        assert!(r.read('?').is_none());
        assert!(r.read('!').is_none());
    }

    #[test]
    fn plus_and_star_alias_clipboard_slot() {
        let mut r = Registers::default();
        r.set_clipboard("payload".into(), false);
        assert_eq!(r.read('+').unwrap().text, "payload");
        assert_eq!(r.read('*').unwrap().text, "payload");
    }

    #[test]
    fn yank_to_plus_writes_clipboard_slot() {
        let mut r = Registers::default();
        r.record_yank("hi".into(), false, Some('+'));
        assert_eq!(r.read('+').unwrap().text, "hi");
        // Unnamed always mirrors the latest write.
        assert_eq!(r.read('"').unwrap().text, "hi");
    }

    #[test]
    fn percent_register_returns_none_when_no_filename() {
        let r = Registers::default();
        assert!(r.read('%').is_none());
    }

    #[test]
    fn percent_register_returns_filename_after_set() {
        let mut r = Registers::default();
        r.set_filename(Some("src/main.rs".into()));
        let slot = r
            .read('%')
            .expect("'%' should return Some after set_filename");
        assert_eq!(slot.text, "src/main.rs");
        assert!(!slot.linewise, "'%' slot should be charwise");
    }

    #[test]
    fn block_yank_flags_slot_blockwise_with_width() {
        let mut r = Registers::default();
        r.record_yank_block("ab\nef".into(), 2, None);
        let s = r.read('"').unwrap();
        assert_eq!(s.text, "ab\nef");
        assert!(s.blockwise);
        assert!(!s.linewise);
        assert_eq!(s.block_width, 2);
        // `"0` mirrors the yank and is blockwise too.
        let z = r.read('0').unwrap();
        assert!(z.blockwise);
        assert_eq!(z.block_width, 2);
    }

    #[test]
    fn block_yank_to_named_carries_kind_and_width() {
        let mut r = Registers::default();
        r.record_yank_block("cd\nkl".into(), 3, Some('a'));
        let a = r.read('a').unwrap();
        assert!(a.blockwise);
        assert_eq!(a.block_width, 3);
        // Unnamed mirrors the named write, kind intact.
        assert!(r.read('"').unwrap().blockwise);
        assert_eq!(r.read('"').unwrap().block_width, 3);
    }

    #[test]
    fn charwise_yank_leaves_slot_non_block() {
        let mut r = Registers::default();
        r.record_yank("plain".into(), false, None);
        let s = r.read('"').unwrap();
        assert!(!s.blockwise);
        assert_eq!(s.block_width, 0);
    }

    #[test]
    fn block_delete_flags_slot_blockwise() {
        let mut r = Registers::default();
        // A blockwise delete spans a newline → routes to the numbered ring.
        r.record_delete_block("a\nb".into(), 1, None);
        let s = r.read('1').unwrap();
        assert!(s.blockwise);
        assert_eq!(s.block_width, 1);
        assert!(r.read('"').unwrap().blockwise);
    }

    #[test]
    fn percent_register_clears_when_set_to_none() {
        let mut r = Registers::default();
        r.set_filename(Some("foo.txt".into()));
        r.set_filename(None);
        assert!(r.read('%').is_none());
    }
}
