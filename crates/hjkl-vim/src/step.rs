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
    pub pre_visual_snapshot: Option<crate::vim::LastVisual>,
}

pub(crate) fn begin_step<H: hjkl_engine::Host>(
    ed: &mut Editor<H>,
    input: hjkl_engine::Input,
) -> Result<StepBookkeeping, bool> {
    use crate::vim::{Mode, Pending};
    use hjkl_engine::input::Key;
    // ── Timestamps ───────────────────────────────────────────────────────
    // Phase 7f: sync buffer before motion handlers see it.
    ed.sync_buffer_content_from_textarea();
    // `:set timeoutlen` chord-timeout handling.
    let now = std::time::Instant::now();
    let host_now = ed.host().now();
    let timed_out = match ed.last_input_host_at() {
        Some(prev) => host_now.saturating_sub(prev) > ed.settings().timeout_len,
        None => false,
    };
    if timed_out {
        let chord_in_flight = !matches!(crate::vim_state::vim(ed).pending, Pending::None)
            || crate::vim_state::vim(ed).count != 0
            || crate::vim_state::vim(ed).pending_register.is_some()
            || crate::vim_state::vim(ed).insert_pending_register;
        if chord_in_flight {
            crate::vim_state::vim_mut(ed).clear_pending_prefix();
        }
    }
    ed.set_last_input_at(Some(now));
    ed.set_last_input_host_at(Some(host_now));
    // ── Macro-stop: bare `q` outside Insert ends the recording ───────────
    if crate::vim_state::vim(ed).recording_macro.is_some()
        && !crate::vim_state::vim(ed).replaying_macro
        && matches!(crate::vim_state::vim(ed).pending, Pending::None)
        && crate::vim_state::vim(ed).mode != Mode::Insert
        && input.key == Key::Char('q')
        && !input.ctrl
        && !input.alt
    {
        let reg = crate::vim_state::vim_mut(ed)
            .recording_macro
            .take()
            .unwrap();
        let keys = std::mem::take(&mut crate::vim_state::vim_mut(ed).recording_keys);
        let text = hjkl_engine::input::encode_macro(&keys);
        ed.set_named_register_text(reg.to_ascii_lowercase(), text);
        return Err(true);
    }
    // ── Snapshots for epilogue ────────────────────────────────────────────
    let pending_was_macro_chord = matches!(
        crate::vim_state::vim(ed).pending,
        Pending::RecordMacroTarget | Pending::PlayMacroTarget { .. }
    );
    let was_insert = crate::vim_state::vim(ed).mode == Mode::Insert;
    let pre_visual_snapshot = match crate::vim_state::vim(ed).mode {
        Mode::Visual => Some(crate::vim::LastVisual {
            mode: Mode::Visual,
            anchor: crate::vim_state::vim(ed).visual_anchor,
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualLine => Some(crate::vim::LastVisual {
            mode: Mode::VisualLine,
            anchor: (crate::vim_state::vim(ed).visual_line_anchor, 0),
            cursor: ed.cursor(),
            block_vcol: 0,
        }),
        Mode::VisualBlock => Some(crate::vim::LastVisual {
            mode: Mode::VisualBlock,
            anchor: crate::vim_state::vim(ed).block_anchor,
            cursor: ed.cursor(),
            block_vcol: crate::vim_state::vim(ed).block_vcol,
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
    use crate::vim::{Mode, Pending};
    use hjkl_engine::input::Key;
    let StepBookkeeping {
        pending_was_macro_chord,
        was_insert,
        pre_visual_snapshot,
    } = bk;
    // ── Visual-exit: set `<`/`>` marks and stash `last_visual` ───────────
    if let Some(snap) = pre_visual_snapshot
        && !matches!(
            crate::vim_state::vim(ed).mode,
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
        crate::vim_state::vim_mut(ed).last_visual = Some(snap);
    }
    // ── Ctrl-o one-shot-normal return to Insert ───────────────────────────
    if !was_insert
        && crate::vim_state::vim(ed).one_shot_normal
        && crate::vim_state::vim(ed).mode == Mode::Normal
        && matches!(crate::vim_state::vim(ed).pending, Pending::None)
    {
        crate::vim_state::vim_mut(ed).one_shot_normal = false;
        crate::vim_state::vim_mut(ed).mode = Mode::Insert;
    }
    // ── Content + viewport sync ───────────────────────────────────────────
    ed.sync_buffer_content_from_textarea();
    if !ed.viewport_pinned() {
        ed.ensure_cursor_in_scrolloff();
    }
    ed.set_viewport_pinned(false);
    // ── Recorder hook ─────────────────────────────────────────────────────
    if crate::vim_state::vim(ed).recording_macro.is_some()
        && !crate::vim_state::vim(ed).replaying_macro
        && input.key != Key::Char('q')
        && !pending_was_macro_chord
    {
        crate::vim_state::vim_mut(ed).recording_keys.push(input);
    }
    // ── Phase 6.3: current_mode sync ─────────────────────────────────────
    crate::vim_state::vim_mut(ed).current_mode = crate::vim_state::vim_mut(ed).public_mode();
    // BLAME is a Normal-only read-only view; any transition out of Normal
    // (a keyboard mode switch, etc.) implicitly leaves it.
    crate::vim::drop_blame_if_left_normal(ed);
    consumed
}
