pub mod cmd;
pub mod count;
pub mod descriptors;
pub mod editor_ext;
pub mod insert;
pub mod motion;
pub mod normal;
pub mod operator;
pub mod pending;
pub mod search_prompt;
mod step;
mod vim_state;

pub use cmd::EngineCmd;
pub use count::CountAccumulator;
pub use editor_ext::VimEditorExt;
// MotionKind moved to hjkl-engine (Phase 6.6 cycle-break); re-exported here for back-compat.
pub use hjkl_engine::MotionKind;
pub use operator::OperatorKind;
pub use pending::{Key, Outcome, PendingState, step};

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
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
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
/// This is the Phase 6.6 entry-point that decouples callers from the engine's
/// internal FSM. Returns `true` if the engine consumed the keystroke.
///
/// # Migration guide
///
/// Replace `editor.step_input(input)` with `hjkl_vim::dispatch_input(&mut editor, input)`.
/// The `Editor::step_input` method is deprecated; remove it in a later release.
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
/// The deprecated `Editor::step_input_raw` shim path is retained for
/// back-compat until Phase 6.6h.
pub fn dispatch_input<H: hjkl_engine::Host>(
    editor: &mut hjkl_engine::Editor<hjkl_buffer::Buffer, H>,
    input: hjkl_engine::Input,
) -> bool {
    // Search-prompt intercept: short-circuits before begin_step, matching
    // vim::step's own search-prompt handling (which also skips begin_step).
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
