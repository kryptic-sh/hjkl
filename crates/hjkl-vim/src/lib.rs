pub mod cmd;
pub mod count;
#[cfg(debug_assertions)]
mod curswant;
pub mod descriptors;
pub mod editor_ext;
pub mod insert;
pub mod motion;
pub mod normal;
pub mod operator;
pub mod pending;
pub mod search_prompt;
mod step;
pub mod vim;
mod vim_state;

pub use cmd::EngineCmd;
pub use count::CountAccumulator;
pub use editor_ext::VimEditorExt;
pub use operator::OperatorKind;
pub use pending::{Key, Outcome, PendingState, step};
/// The byte budget one `p` / `P` may insert. Public so a host can report the
/// limit it just hit rather than restating the number.
pub use vim::command::MAX_PASTE_BYTES;
/// Build an `Editor` that interprets keys as vim, or retro-fit the discipline
/// onto one that already exists.
///
/// `Editor::new` leaves the discipline slot empty (the engine cannot name a
/// concrete discipline), so an editor built through it ignores vim keys. Every
/// vim-driven editor goes through one of these two (#267).
pub use vim::{install as install_vim_discipline, vim_editor};

/// Mode discriminator for the hjkl editor stack.
///
/// Used as the mode parameter in `hjkl-keymap`'s generic `Keymap<A, M: Mode>`.
/// Satisfies the `hjkl_keymap::Mode` trait via its blanket impl for any
/// `Copy + Eq + Hash + Debug` type.
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

/// Drive the vim FSM with a [`hjkl_engine::PlannedInput`]. Translates the
/// planned input to engine [`hjkl_engine::Input`], dispatches through
/// [`dispatch_input`], and emits cursor-shape changes.
///
/// Returns `true` if the engine consumed the keystroke. Returns `false` for
/// variants the legacy FSM does not dispatch (`Mouse`, `Paste`, `FocusGained`,
/// `FocusLost`, `Resize`) and for special-key variants that map to `Key::Null`.
pub fn feed_input<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: hjkl_engine::PlannedInput,
) -> bool {
    let Some(event) = hjkl_engine::decode_planned_input(input) else {
        return false;
    };
    let consumed = dispatch_input(editor, event);
    editor.emit_cursor_shape_if_changed();
    consumed
}

/// Drive the vim FSM with one [`hjkl_engine::Input`].
///
/// This is the sole entry-point that decouples callers from the engine's
/// internal FSM. Returns `true` if the engine consumed the keystroke.
///
/// # Phase 6.6c / 6.6d / 6.6e
///
/// Search-prompt mode (6.6c) is intercepted here before `begin_step` because
/// it is a true short-circuit (no prelude/epilogue needed).
///
/// Insert mode (6.6d) is hosted in `hjkl-vim::insert::step_insert`.
///
/// Normal / Visual / VisualLine / VisualBlock / operator-pending modes (6.6e)
/// are hosted in `hjkl-vim::normal::step_normal`. Both are wrapped with
/// `begin_step` / `end_step` so macro recording, viewport scrolling, and
/// `current_mode` sync all fire correctly.
///
/// In debug builds this is also where the `curswant` invariant is checked —
/// see [`crate::curswant`]. Every keystroke that reaches the vim FSM passes
/// through here, from the app (`hjkl_vim_tui::handle_key`), the compat-oracle
/// driver, `:normal`, and macro replay alike, which makes it the one place
/// the check has to live.
pub fn dispatch_input<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: hjkl_engine::Input,
) -> bool {
    #[cfg(debug_assertions)]
    let pre = curswant::capture(editor);
    let consumed = dispatch_input_inner(editor, input);
    #[cfg(debug_assertions)]
    curswant::assert_invariant(editor, pre, input);
    consumed
}

fn dispatch_input_inner<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::View, H>,
    input: hjkl_engine::Input,
) -> bool {
    // Search-prompt intercept: short-circuits before begin_step because it
    // needs no prelude/epilogue.
    if editor.search_prompt_state().is_some() {
        return search_prompt::step_search_prompt(editor, input);
    }
    // Run the prelude (timestamps, chord-timeout, macro-stop, snapshots).
    let bk = match step::begin_step(editor, input) {
        Ok(bk) => bk,
        Err(consumed) => return consumed,
    };
    // Per-mode FSM dispatch — hjkl-vim hosts all modes.
    let consumed = match editor.vim_mode() {
        hjkl_engine::VimMode::Insert => insert::step_insert(editor, input),
        _ => normal::step_normal(editor, input),
    };
    // Run the epilogue (marks, one-shot-normal, sync, recorder, mode sync).
    step::end_step(editor, input, bk, consumed)
}
