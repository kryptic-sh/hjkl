//! Per-keystroke prelude/epilogue bookkeeping (#267): relocated from
//! `hjkl-engine`'s `Editor::begin_step`/`end_step`. Operates on the engine
//! `Editor` via its public surface + the transitional `pub vim` field.

use hjkl_engine::Editor as EngineEditor;
type Editor<H> = EngineEditor<hjkl_buffer::Buffer, H>;

pub struct StepBookkeeping {
    /// True when the pending chord before this step was a macro-chord
    /// (`q{reg}` or `@{reg}`). The recorder hook skips these bookkeeping
    /// keys so that only the *payload* keys enter `recording_keys`.
    pub pending_was_macro_chord: bool,
    /// True when the mode was Insert *before* the FSM body ran. Used by
    /// the Ctrl-o one-shot-normal epilogue to decide whether to bounce
    /// back into Insert.
    pub was_insert: bool,
    /// Pre-dispatch visual snapshot. When the FSM body transitions out of
    /// a visual mode the epilogue uses this to set the `<`/`>` marks and
    /// store `last_visual` for `gv`.
    pub pre_visual_snapshot: Option<hjkl_engine::vim::LastVisual>,
}

pub(crate) fn begin_step<H: hjkl_engine::Host>(
    ed: &mut Editor<H>,
    input: hjkl_engine::Input,
) -> Result<StepBookkeeping, bool> {
    use hjkl_engine::input::Key;
    use hjkl_engine::vim::{Mode, Pending};
    // ── Timestamps ───────────────────────────────────────────────────────
    // Phase 7f: sync buffer before motion handlers see it.
    ed.sync_buffer_content_from_textarea();
    // `:set timeoutlen` chord-timeout handling.
    let now = std::time::Instant::now();
    let host_now = ed.host().now();
    let timed_out = match ed.vim.last_input_host_at {
        Some(prev) => host_now.saturating_sub(prev) > ed.settings().timeout_len,
        None => false,
    };
    if timed_out {
        let chord_in_flight = !matches!(ed.vim.pending, Pending::None)
            || ed.vim.count != 0
            || ed.vim.pending_register.is_some()
            || ed.vim.insert_pending_register;
        if chord_in_flight {
            ed.vim.clear_pending_prefix();
        }
    }
    ed.vim.last_input_at = Some(now);
    ed.vim.last_input_host_at = Some(host_now);
    // ── Macro-stop: bare `q` outside Insert ends the recording ───────────
    if ed.vim.recording_macro.is_some()
        && !ed.vim.replaying_macro
        && matches!(ed.vim.pending, Pending::None)
        && ed.vim.mode != Mode::Insert
        && input.key == Key::Char('q')
        && !input.ctrl
        && !input.alt
    {
        let reg = ed.vim.recording_macro.take().unwrap();
        let keys = std::mem::take(&mut ed.vim.recording_keys);
        let text = hjkl_engine::input::encode_macro(&keys);
        ed.set_named_register_text(reg.to_ascii_lowercase(), text);
        return Err(true);
    }
    // ── Snapshots for epilogue ────────────────────────────────────────────
    let pending_was_macro_chord = matches!(
        ed.vim.pending,
        Pending::RecordMacroTarget | Pending::PlayMacroTarget { .. }
    );
    let was_insert = ed.vim.mode == Mode::Insert;
    let pre_visual_snapshot = match ed.vim.mode {
        Mode::Visual => Some(hjkl_engine::vim::LastVisual {
            mode: Mode::Visual,
            anchor: ed.vim.visual_anchor,
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualLine => Some(hjkl_engine::vim::LastVisual {
            mode: Mode::VisualLine,
            anchor: (ed.vim.visual_line_anchor, 0),
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualBlock => Some(hjkl_engine::vim::LastVisual {
            mode: Mode::VisualBlock,
            anchor: ed.vim.block_anchor,
            cursor: ed.cursor(),
            block_vcol: ed.vim.block_vcol,
        }),
        _ => None,
    };
    Ok(StepBookkeeping {
        pending_was_macro_chord,
        was_insert,
        pre_visual_snapshot,
    })
}

pub(crate) fn end_step<H: hjkl_engine::Host>(
    ed: &mut Editor<H>,
    input: hjkl_engine::Input,
    bk: StepBookkeeping,
    consumed: bool,
) -> bool {
    use hjkl_engine::input::Key;
    use hjkl_engine::vim::{Mode, Pending};
    let StepBookkeeping {
        pending_was_macro_chord,
        was_insert,
        pre_visual_snapshot,
    } = bk;
    // ── Visual-exit: set `<`/`>` marks and stash `last_visual` ───────────
    if let Some(snap) = pre_visual_snapshot
        && !matches!(
            ed.vim.mode,
            Mode::Visual | Mode::VisualLine | Mode::VisualBlock
        )
    {
        let (lo, hi) = match snap.mode {
            Mode::Visual => {
                if snap.anchor <= snap.cursor {
                    (snap.anchor, snap.cursor)
                } else {
                    (snap.cursor, snap.anchor)
                }
            }
            Mode::VisualLine => {
                let r_lo = snap.anchor.0.min(snap.cursor.0);
                let r_hi = snap.anchor.0.max(snap.cursor.0);
                let vl_rope = ed.buffer().rope();
                let r_hi_clamped = r_hi.min(vl_rope.len_lines().saturating_sub(1));
                let last_col = hjkl_buffer::rope_line_str(&vl_rope, r_hi_clamped)
                    .chars()
                    .count()
                    .saturating_sub(1);
                ((r_lo, 0), (r_hi, last_col))
            }
            Mode::VisualBlock => {
                let (r1, c1) = snap.anchor;
                let (r2, c2) = snap.cursor;
                ((r1.min(r2), c1.min(c2)), (r1.max(r2), c1.max(c2)))
            }
            _ => {
                if snap.anchor <= snap.cursor {
                    (snap.anchor, snap.cursor)
                } else {
                    (snap.cursor, snap.anchor)
                }
            }
        };
        ed.set_mark('<', lo);
        ed.set_mark('>', hi);
        ed.vim.last_visual = Some(snap);
    }
    // ── Ctrl-o one-shot-normal return to Insert ───────────────────────────
    if !was_insert
        && ed.vim.one_shot_normal
        && ed.vim.mode == Mode::Normal
        && matches!(ed.vim.pending, Pending::None)
    {
        ed.vim.one_shot_normal = false;
        ed.vim.mode = Mode::Insert;
    }
    // ── Content + viewport sync ───────────────────────────────────────────
    ed.sync_buffer_content_from_textarea();
    if !ed.vim.viewport_pinned {
        ed.ensure_cursor_in_scrolloff();
    }
    ed.vim.viewport_pinned = false;
    // ── Recorder hook ─────────────────────────────────────────────────────
    if ed.vim.recording_macro.is_some()
        && !ed.vim.replaying_macro
        && input.key != Key::Char('q')
        && !pending_was_macro_chord
    {
        ed.vim.recording_keys.push(input);
    }
    // ── Phase 6.3: current_mode sync ─────────────────────────────────────
    ed.vim.current_mode = ed.vim.public_mode();
    // BLAME is a Normal-only read-only view; any transition out of Normal
    // (a keyboard mode switch, etc.) implicitly leaves it.
    hjkl_engine::vim::drop_blame_if_left_normal(ed);
    consumed
}
