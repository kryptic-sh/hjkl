/// Mode discriminator for the hjkl editor stack.
///
/// Used as the mode parameter in `hjkl-keymap`'s generic `Keymap<A, M: Mode>`.
/// Satisfies the `hjkl_keymap::Mode` trait via its blanket impl for any
/// `Copy + Eq + Hash + Debug` type.
///
/// Phase 2+ will move the vim FSM itself here. For now this is pure plumbing:
/// the enum lives in `hjkl-vim` so future FSM work has a stable home.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
    OpPending,
    CommandLine,
}
