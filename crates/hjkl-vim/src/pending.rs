/// Pending-state machine for second-key chords. The umbrella stores
/// `Option<PendingState>`; when `Some`, it routes keys through `step`
/// instead of the keymap trie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    Replace {
        count: usize,
    },
    /// `f<x>` / `F<x>` / `t<x>` / `T<x>` — find single char on current line.
    /// `forward` = direction (true for f/t, false for F/T).
    /// `till` = stop one char before target (true for t/T, false for f/F).
    Find {
        count: usize,
        forward: bool,
        till: bool,
    },
    /// `g<x>` — bare g-prefix chord in Normal / Visual mode. The app sets this
    /// after intercepting `g`; `step` routes the next `Key::Char(ch)` to
    /// `EngineCmd::AfterGChord { ch, count }`. `Key::Esc` cancels; any
    /// non-char key also cancels (mirrors the `Find` arm).
    AfterG {
        count: usize,
    },
    /// `z<x>` — bare z-prefix chord in Normal / Visual mode. The app sets this
    /// after intercepting `z`; `step` routes the next `Key::Char(ch)` to
    /// `EngineCmd::AfterZChord { ch, count }`. `Key::Esc` cancels; any
    /// non-char key also cancels (mirrors the `AfterG` arm).
    AfterZ {
        count: usize,
    },
    /// `d<x>` / `y<x>` / `c<x>` / `><x>` / `<<x>` — bare op-pending entered
    /// from Normal mode after the operator key. `count1` is the count pressed
    /// before the operator; `inner_count` accumulates digits pressed after the
    /// operator (e.g. `d3w` → count1=1, inner_count=3, total=3). The reducer
    /// is authoritative for both counts; `total = count1.max(1) *
    /// inner_count.max(1)` is passed to the engine on completion.
    ///
    /// Vim quirk: a bare `0` when `inner_count == 0` is the line-start motion
    /// (`LineStart`), not a digit. Any other digit, or `0` when `inner_count >
    /// 0`, accumulates.
    AfterOp {
        op: crate::operator::OperatorKind,
        count1: usize,
        inner_count: usize,
    },
    /// `df<x>` / `dF<x>` / `dt<x>` / `dT<x>` and same for y/c/>/<. Reached
    /// from `AfterOp` when the next key after the operator is `f`/`F`/`t`/`T`.
    /// `total_count = count1.max(1) * inner_count.max(1)` already folded at
    /// transition time; neither component is independently meaningful after
    /// this point.
    ///
    /// The next char is the find target. `Key::Esc` or any non-char cancels
    /// (vim's `f<Esc>` cancel semantics apply here too).
    ///
    /// `cf<x>` stays as Change + Find — the cw→ce quirk in `apply_op_with_motion`
    /// only rewrites `Motion::WordFwd`/`BigWordFwd`, not `Motion::Find`.
    OpFind {
        op: crate::operator::OperatorKind,
        total_count: usize,
        forward: bool,
        till: bool,
    },
    /// `di<x>` / `da<x>` etc. — reached from `AfterOp` when next key after
    /// operator is `i` or `a`. `total_count = count1 * inner_count` already
    /// folded; engine ignores it for text-object motions but it's passed
    /// through for future-proofing / consistency with `OpFind` shape.
    OpTextObj {
        op: crate::operator::OperatorKind,
        total_count: usize,
        inner: bool,
    },
    /// `dgg` / `dge` / `dgE` / `dgj` / `dgk` etc. — reached from `AfterOp`
    /// when next key after operator is `g`. For case-ops (gu/gU/g~) the
    /// doubled form (gUgU = gUU linewise) is dispatched here too — engine
    /// detects via op-matching second char.
    OpG {
        op: crate::operator::OperatorKind,
        total_count: usize,
    },
    /// `"<reg>` — register-prefix chord in Normal mode. The next char names
    /// a register that the next y/d/c/p operation will use. Engine validates
    /// the char; invalid chars silently no-op.
    SelectRegister,
    /// `m<x>` — set mark `x` at current cursor position. Any char cancels on
    /// Esc or non-char key; only alphanumeric and special marks are accepted by
    /// the engine, invalid chars silently no-op (engine validates).
    SetMark,
    /// `'<x>` — go to mark `x`, linewise (row only, col = first non-blank).
    /// Esc or non-char key cancels; engine validates the char and no-ops on
    /// unset or invalid marks.
    GotoMarkLine,
    /// `` `<x> `` — go to mark `x`, charwise (row + col). Esc or non-char key
    /// cancels; engine validates the char and no-ops on unset or invalid marks.
    GotoMarkChar,
    /// `q` pressed in Normal mode while NOT already recording — waits for the
    /// register char. Esc or non-char key cancels (no recording started). Any
    /// alphabetic or digit char commits `StartMacroRecord { reg: ch }`. The
    /// stop-on-bare-`q` path is handled in `AppAction::QChord` BEFORE this
    /// pending state is entered.
    RecordMacroTarget,
    /// `@` pressed in Normal mode — waits for the register char. Esc or
    /// non-char key cancels. `'@'` commits `PlayMacro { reg: '@', count }` for
    /// `@@` repeat-last semantics (host resolves actual register). `':'`
    /// commits `PlayMacro { reg: ':', count }` for `@:` last-ex-repeat
    /// (host handles app-side storage — Phase 5d). Any other alphabetic or
    /// digit char commits `PlayMacro { reg: ch, count }`.
    PlayMacroTarget {
        count: usize,
    },
}

/// One step of the reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Need more keys — keep accumulating with new state.
    Wait(PendingState),
    /// Run this engine command, then clear pending.
    Commit(crate::cmd::EngineCmd),
    /// Cancel pending (Esc, invalid char, etc.). No engine call.
    Cancel,
    /// Pending state didn't consume this key — host should route it
    /// normally (e.g. modifier-only key). Pending state stays alive.
    Forward,
}

/// `Key` is intentionally minimal — hjkl-vim should not depend on
/// crossterm. Hosts translate their native keys into this shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    // Add more variants only as later chunks require them.
}

pub fn step(state: PendingState, key: Key) -> Outcome {
    match state {
        PendingState::Replace { count } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::ReplaceChar { ch, count }),
            Key::Enter => Outcome::Commit(crate::cmd::EngineCmd::ReplaceChar { ch: '\n', count }),
            _ => Outcome::Cancel,
        },
        PendingState::Find {
            count,
            forward,
            till,
        } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::FindChar {
                ch,
                forward,
                till,
                count,
            }),
            // Any non-char key cancels (vim cancels f<non-char>).
            _ => Outcome::Cancel,
        },
        PendingState::AfterG { count } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::AfterGChord { ch, count }),
            // Any non-char key cancels (mirrors Find arm).
            _ => Outcome::Cancel,
        },
        PendingState::AfterZ { count } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::AfterZChord { ch, count }),
            // Any non-char key cancels (mirrors AfterG arm).
            _ => Outcome::Cancel,
        },
        PendingState::AfterOp {
            op,
            count1,
            inner_count,
        } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(d @ '0'..='9') => {
                // Vim quirk: bare `0` with inner_count==0 is LineStart motion.
                if d == '0' && inner_count == 0 {
                    // Treat as motion key — engine will parse '0' as LineStart.
                    let total = count1.max(1);
                    Outcome::Commit(crate::cmd::EngineCmd::ApplyOpMotion {
                        op,
                        motion_key: '0',
                        total_count: total,
                    })
                } else {
                    let new_inner = inner_count
                        .saturating_mul(10)
                        .saturating_add(d as usize - '0' as usize);
                    Outcome::Wait(PendingState::AfterOp {
                        op,
                        count1,
                        inner_count: new_inner,
                    })
                }
            }
            Key::Char(ch) => {
                let total = count1.max(1).saturating_mul(inner_count.max(1));
                // Doubled letter → line op (dd/yy/cc/>>/<<).
                if ch == op.double_char() {
                    Outcome::Commit(crate::cmd::EngineCmd::ApplyOpDouble {
                        op,
                        total_count: total,
                    })
                // Text object: `i` → inner, `a` → outer. Transition to
                // `OpTextObj` so the reducer owns the next char instead of
                // delegating to the engine FSM (mirrors OpFind pattern).
                } else if ch == 'i' {
                    Outcome::Wait(PendingState::OpTextObj {
                        op,
                        total_count: total,
                        inner: true,
                    })
                } else if ch == 'a' {
                    Outcome::Wait(PendingState::OpTextObj {
                        op,
                        total_count: total,
                        inner: false,
                    })
                // g-chord sub-pending (dgg, dge, etc.): transition to OpG so
                // the reducer owns the second char instead of delegating to the
                // engine FSM. `total_count` collapses both counts at transition
                // time (mirrors OpFind / OpTextObj pattern).
                } else if ch == 'g' {
                    Outcome::Wait(PendingState::OpG {
                        op,
                        total_count: total,
                    })
                // Find sub-pending (df/dF/dt/dT): transition to OpFind instead
                // of setting engine Pending::OpFind. `total_count` collapses
                // both counts at transition time.
                } else if ch == 'f' {
                    Outcome::Wait(PendingState::OpFind {
                        op,
                        total_count: total,
                        forward: true,
                        till: false,
                    })
                } else if ch == 'F' {
                    Outcome::Wait(PendingState::OpFind {
                        op,
                        total_count: total,
                        forward: false,
                        till: false,
                    })
                } else if ch == 't' {
                    Outcome::Wait(PendingState::OpFind {
                        op,
                        total_count: total,
                        forward: true,
                        till: true,
                    })
                } else if ch == 'T' {
                    Outcome::Wait(PendingState::OpFind {
                        op,
                        total_count: total,
                        forward: false,
                        till: true,
                    })
                } else {
                    // All other chars: treat as motion key and let the engine
                    // parse it via parse_motion. Unknown keys no-op in the engine.
                    Outcome::Commit(crate::cmd::EngineCmd::ApplyOpMotion {
                        op,
                        motion_key: ch,
                        total_count: total,
                    })
                }
            }
            // Non-char, non-Esc → cancel (mirrors Find/AfterG arms).
            _ => Outcome::Cancel,
        },
        PendingState::OpFind {
            op,
            total_count,
            forward,
            till,
        } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::ApplyOpFind {
                op,
                ch,
                forward,
                till,
                total_count,
            }),
            // Any non-char key cancels (vim's f<non-char> cancel semantics apply).
            _ => Outcome::Cancel,
        },
        PendingState::OpTextObj {
            op,
            total_count,
            inner,
        } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::ApplyOpTextObj {
                op,
                ch,
                inner,
                total_count,
            }),
            // Any non-char key cancels; engine handles invalid chars as no-ops.
            _ => Outcome::Cancel,
        },
        PendingState::OpG { op, total_count } => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::ApplyOpG {
                op,
                ch,
                total_count,
            }),
            // Any non-char key cancels; engine apply_op_g handles unknown chars
            // as a no-op (mirrors OpTextObj arm).
            _ => Outcome::Cancel,
        },
        PendingState::SelectRegister => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::SetPendingRegister { reg: ch }),
            // Any non-char key cancels (mirrors AfterG / Find arms).
            _ => Outcome::Cancel,
        },
        PendingState::SetMark => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::SetMark { ch }),
            // Any non-char key cancels (mirrors SelectRegister / AfterG arms).
            _ => Outcome::Cancel,
        },
        PendingState::GotoMarkLine => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::GotoMarkLine { ch }),
            // Any non-char key cancels (mirrors SetMark arm).
            _ => Outcome::Cancel,
        },
        PendingState::GotoMarkChar => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) => Outcome::Commit(crate::cmd::EngineCmd::GotoMarkChar { ch }),
            // Any non-char key cancels (mirrors GotoMarkLine arm).
            _ => Outcome::Cancel,
        },
        PendingState::RecordMacroTarget => match key {
            Key::Esc => Outcome::Cancel,
            Key::Char(ch) if ch.is_ascii_alphabetic() || ch.is_ascii_digit() => {
                Outcome::Commit(crate::cmd::EngineCmd::StartMacroRecord { reg: ch })
            }
            // Non-alphabetic/digit char or non-char key cancels (no recording started).
            _ => Outcome::Cancel,
        },
        PendingState::PlayMacroTarget { count } => match key {
            Key::Esc => Outcome::Cancel,
            // `@@` — repeat-last semantics; pass literal '@' and let the host resolve.
            Key::Char('@') => Outcome::Commit(crate::cmd::EngineCmd::PlayMacro { reg: '@', count }),
            // `@:` — last-ex-repeat; host handles app-side storage (Phase 5d).
            Key::Char(':') => Outcome::Commit(crate::cmd::EngineCmd::PlayMacro { reg: ':', count }),
            Key::Char(ch) if ch.is_ascii_alphabetic() || ch.is_ascii_digit() => {
                Outcome::Commit(crate::cmd::EngineCmd::PlayMacro { reg: ch, count })
            }
            // Any other char or non-char key cancels.
            _ => Outcome::Cancel,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::EngineCmd;
    use crate::operator::OperatorKind;

    // ── AfterG reducer unit tests ────────────────────────────────────────────

    #[test]
    fn after_g_gg_commits() {
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(
            step(state, Key::Char('g')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: 'g', count: 1 })
        );
    }

    #[test]
    fn after_g_gv_commits() {
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(
            step(state, Key::Char('v')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: 'v', count: 1 })
        );
    }

    #[test]
    fn after_g_gu_operator_commits() {
        // gU still produces AfterGChord; the engine handles the Pending::Op transition.
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(
            step(state, Key::Char('U')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: 'U', count: 1 })
        );
    }

    #[test]
    fn after_g_gi_commits() {
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(
            step(state, Key::Char('i')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: 'i', count: 1 })
        );
    }

    #[test]
    fn after_g_esc_cancels() {
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn after_g_count_carry_through() {
        // 5gg enters with count=5 — AfterGChord carries it through.
        let state = PendingState::AfterG { count: 5 };
        assert_eq!(
            step(state, Key::Char('g')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: 'g', count: 5 })
        );
    }

    #[test]
    fn after_g_non_char_cancels() {
        // Non-char, non-Esc key (e.g. Enter) cancels.
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn g_ampersand_dispatches_via_g_chord() {
        // `g&` must emit AfterGChord { ch: '&', count: 1 }, not be treated as
        // the standalone `&` substitute-char.
        let state = PendingState::AfterG { count: 1 };
        assert_eq!(
            step(state, Key::Char('&')),
            Outcome::Commit(EngineCmd::AfterGChord { ch: '&', count: 1 })
        );
    }

    // ── AfterZ reducer unit tests ────────────────────────────────────────────

    #[test]
    fn after_z_zz_commits() {
        let state = PendingState::AfterZ { count: 1 };
        assert_eq!(
            step(state, Key::Char('z')),
            Outcome::Commit(EngineCmd::AfterZChord { ch: 'z', count: 1 })
        );
    }

    #[test]
    fn after_z_zf_commits() {
        let state = PendingState::AfterZ { count: 1 };
        assert_eq!(
            step(state, Key::Char('f')),
            Outcome::Commit(EngineCmd::AfterZChord { ch: 'f', count: 1 })
        );
    }

    #[test]
    fn after_z_esc_cancels() {
        let state = PendingState::AfterZ { count: 1 };
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn after_z_count_carry_through() {
        // 3zz enters with count=3 — AfterZChord carries it through.
        let state = PendingState::AfterZ { count: 3 };
        assert_eq!(
            step(state, Key::Char('z')),
            Outcome::Commit(EngineCmd::AfterZChord { ch: 'z', count: 3 })
        );
    }

    #[test]
    fn after_z_non_char_cancels() {
        // Non-char, non-Esc key (e.g. Enter) cancels.
        let state = PendingState::AfterZ { count: 1 };
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── AfterOp reducer unit tests ───────────────────────────────────────────

    fn after_op(op: OperatorKind, count1: usize) -> PendingState {
        PendingState::AfterOp {
            op,
            count1,
            inner_count: 0,
        }
    }

    #[test]
    fn op_d_then_w_commits_motion() {
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: 'w',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_d_commits_double() {
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('d')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Delete,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_inner_count_d3w_commits_motion_with_count_3() {
        // d3w: count1=1, inner_count accumulates to 3, total=3.
        let state = after_op(OperatorKind::Delete, 1);
        // Type '3'.
        let Outcome::Wait(state2) = step(state, Key::Char('3')) else {
            panic!("expected Wait");
        };
        assert_eq!(
            state2,
            PendingState::AfterOp {
                op: OperatorKind::Delete,
                count1: 1,
                inner_count: 3
            }
        );
        // Type 'w'.
        assert_eq!(
            step(state2, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: 'w',
                total_count: 3,
            })
        );
    }

    #[test]
    fn op_2d_d_commits_double_with_count_2() {
        // 2dd: count1=2, inner_count=0, doubled → total=2.
        let state = after_op(OperatorKind::Delete, 2);
        assert_eq!(
            step(state, Key::Char('d')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Delete,
                total_count: 2,
            })
        );
    }

    #[test]
    fn op_2d_3w_commits_motion_with_total_6() {
        // 2d3w: count1=2, inner=3, total=6.
        let state = after_op(OperatorKind::Delete, 2);
        let Outcome::Wait(state2) = step(state, Key::Char('3')) else {
            panic!("expected Wait");
        };
        assert_eq!(
            step(state2, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: 'w',
                total_count: 6,
            })
        );
    }

    #[test]
    fn op_d_then_i_transitions_to_op_text_obj_inner() {
        // `di` → Wait(OpTextObj { inner:true, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('i')),
            Outcome::Wait(PendingState::OpTextObj {
                op: OperatorKind::Delete,
                total_count: 1,
                inner: true,
            })
        );
    }

    #[test]
    fn op_d_then_a_transitions_to_op_text_obj_around() {
        // `da` → Wait(OpTextObj { inner:false, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Wait(PendingState::OpTextObj {
                op: OperatorKind::Delete,
                total_count: 1,
                inner: false,
            })
        );
    }

    #[test]
    fn op_d_then_g_transitions_to_op_g() {
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('g')),
            Outcome::Wait(PendingState::OpG {
                op: OperatorKind::Delete,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_f_transitions_to_op_find_forward_not_till() {
        // `df` → Wait(OpFind { forward:true, till:false, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('f')),
            Outcome::Wait(PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: 1,
                forward: true,
                till: false,
            })
        );
    }

    #[test]
    fn op_d_then_cap_f_transitions_to_op_find_backward_not_till() {
        // `dF` → Wait(OpFind { forward:false, till:false, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('F')),
            Outcome::Wait(PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: 1,
                forward: false,
                till: false,
            })
        );
    }

    #[test]
    fn op_d_then_t_transitions_to_op_find_forward_till() {
        // `dt` → Wait(OpFind { forward:true, till:true, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('t')),
            Outcome::Wait(PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: 1,
                forward: true,
                till: true,
            })
        );
    }

    #[test]
    fn op_d_then_cap_t_transitions_to_op_find_backward_till() {
        // `dT` → Wait(OpFind { forward:false, till:true, total_count:1 })
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('T')),
            Outcome::Wait(PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: 1,
                forward: false,
                till: true,
            })
        );
    }

    // ── OpFind reducer unit tests ────────────────────────────────────────────

    fn op_find(op: OperatorKind, total_count: usize, forward: bool, till: bool) -> PendingState {
        PendingState::OpFind {
            op,
            total_count,
            forward,
            till,
        }
    }

    #[test]
    fn op_d_then_f_then_x_commits_apply_op_find() {
        // `dfx` → ApplyOpFind { Delete, 'x', forward:true, till:false, total:1 }
        let state = op_find(OperatorKind::Delete, 1, true, false);
        assert_eq!(
            step(state, Key::Char('x')),
            Outcome::Commit(EngineCmd::ApplyOpFind {
                op: OperatorKind::Delete,
                ch: 'x',
                forward: true,
                till: false,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_cap_f_then_x_commits_apply_op_find_backward() {
        // `dFx` → ApplyOpFind { Delete, 'x', forward:false, till:false, total:1 }
        let state = op_find(OperatorKind::Delete, 1, false, false);
        assert_eq!(
            step(state, Key::Char('x')),
            Outcome::Commit(EngineCmd::ApplyOpFind {
                op: OperatorKind::Delete,
                ch: 'x',
                forward: false,
                till: false,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_t_then_x_commits_apply_op_find_till() {
        // `dtx` → ApplyOpFind { Delete, 'x', forward:true, till:true, total:1 }
        let state = op_find(OperatorKind::Delete, 1, true, true);
        assert_eq!(
            step(state, Key::Char('x')),
            Outcome::Commit(EngineCmd::ApplyOpFind {
                op: OperatorKind::Delete,
                ch: 'x',
                forward: true,
                till: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_cap_t_then_x_commits_apply_op_find_backward_till() {
        // `dTx` → ApplyOpFind { Delete, 'x', forward:false, till:true, total:1 }
        let state = op_find(OperatorKind::Delete, 1, false, true);
        assert_eq!(
            step(state, Key::Char('x')),
            Outcome::Commit(EngineCmd::ApplyOpFind {
                op: OperatorKind::Delete,
                ch: 'x',
                forward: false,
                till: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_2d_3f_x_commits_total_count_6() {
        // `2d3fx`: count1=2, inner_count=3 → total=6 folded at AfterOp→OpFind.
        // Simulate via AfterOp(count1=2, inner_count=3) then 'f', then 'x'.
        let state = PendingState::AfterOp {
            op: OperatorKind::Delete,
            count1: 2,
            inner_count: 3,
        };
        let Outcome::Wait(op_find_state) = step(state, Key::Char('f')) else {
            panic!("expected Wait(OpFind)");
        };
        assert_eq!(
            op_find_state,
            PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: 6,
                forward: true,
                till: false,
            }
        );
        assert_eq!(
            step(op_find_state, Key::Char('x')),
            Outcome::Commit(EngineCmd::ApplyOpFind {
                op: OperatorKind::Delete,
                ch: 'x',
                forward: true,
                till: false,
                total_count: 6,
            })
        );
    }

    #[test]
    fn op_d_f_then_esc_cancels() {
        // `df<Esc>` — vim cancels f<Esc>, so OpFind on Esc → Cancel.
        let state = op_find(OperatorKind::Delete, 1, true, false);
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn op_d_f_then_enter_cancels() {
        // Non-char key after `df` cancels (mirrors Find arm).
        let state = op_find(OperatorKind::Delete, 1, true, false);
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn op_d_then_esc_cancels() {
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn op_d_non_char_cancels() {
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── OpTextObj reducer unit tests ─────────────────────────────────────────

    fn op_text_obj(op: OperatorKind, total_count: usize, inner: bool) -> PendingState {
        PendingState::OpTextObj {
            op,
            total_count,
            inner,
        }
    }

    #[test]
    fn op_d_then_i_then_w_commits_apply_op_text_obj_inner() {
        // `diw` → ApplyOpTextObj { Delete, 'w', inner:true, total_count:1 }
        let state = op_text_obj(OperatorKind::Delete, 1, true);
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Delete,
                ch: 'w',
                inner: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_a_then_w_commits_apply_op_text_obj_around() {
        // `daw` → ApplyOpTextObj { Delete, 'w', inner:false, total_count:1 }
        let state = op_text_obj(OperatorKind::Delete, 1, false);
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Delete,
                ch: 'w',
                inner: false,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_i_then_quote_commits_with_quote_char() {
        // `di"` → ApplyOpTextObj { Delete, '"', inner:true, total_count:1 }
        let state = op_text_obj(OperatorKind::Delete, 1, true);
        assert_eq!(
            step(state, Key::Char('"')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Delete,
                ch: '"',
                inner: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_i_then_paren_commits_with_paren() {
        // `di(` → ApplyOpTextObj { Delete, '(', inner:true, total_count:1 }
        let state = op_text_obj(OperatorKind::Delete, 1, true);
        assert_eq!(
            step(state, Key::Char('(')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Delete,
                ch: '(',
                inner: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_c_then_i_then_p_commits_change_paragraph_inner() {
        // `cip` → ApplyOpTextObj { Change, 'p', inner:true, total_count:1 }
        let state = op_text_obj(OperatorKind::Change, 1, true);
        assert_eq!(
            step(state, Key::Char('p')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Change,
                ch: 'p',
                inner: true,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_i_then_esc_cancels() {
        // `di<Esc>` — Esc after OpTextObj transition cancels.
        let state = op_text_obj(OperatorKind::Delete, 1, true);
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn op_d_i_then_enter_cancels() {
        // Non-char key after `di` cancels (mirrors OpFind arm).
        let state = op_text_obj(OperatorKind::Delete, 1, true);
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn op_2d_i_w_total_count_2_preserved() {
        // `2diw`: count1=2, inner_count=0 → total=2. Check count carry-through.
        // Simulate via AfterOp(count1=2, inner_count=0) then 'i', then 'w'.
        let state = PendingState::AfterOp {
            op: OperatorKind::Delete,
            count1: 2,
            inner_count: 0,
        };
        let Outcome::Wait(obj_state) = step(state, Key::Char('i')) else {
            panic!("expected Wait(OpTextObj)");
        };
        assert_eq!(
            obj_state,
            PendingState::OpTextObj {
                op: OperatorKind::Delete,
                total_count: 2,
                inner: true,
            }
        );
        assert_eq!(
            step(obj_state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Delete,
                ch: 'w',
                inner: true,
                total_count: 2,
            })
        );
    }

    #[test]
    fn op_d_bare_zero_is_line_start_motion() {
        // Bare '0' with inner_count=0 → LineStart motion (total=1).
        let state = after_op(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('0')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: '0',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_zero_accumulates_when_inner_count_nonzero() {
        // d10w: '1' accumulates to inner=1, then '0' accumulates (inner>0) to inner=10.
        let state = after_op(OperatorKind::Delete, 1);
        let Outcome::Wait(s2) = step(state, Key::Char('1')) else {
            panic!("expected Wait");
        };
        let Outcome::Wait(s3) = step(s2, Key::Char('0')) else {
            panic!("expected Wait");
        };
        assert_eq!(
            s3,
            PendingState::AfterOp {
                op: OperatorKind::Delete,
                count1: 1,
                inner_count: 10,
            }
        );
        assert_eq!(
            step(s3, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: 'w',
                total_count: 10,
            })
        );
    }

    #[test]
    fn op_total_count_saturates_instead_of_overflowing() {
        // Pathological counts: count1 * inner_count must saturate, not
        // overflow (overflow panics in debug builds).
        let state = PendingState::AfterOp {
            op: OperatorKind::Delete,
            count1: usize::MAX,
            inner_count: 2,
        };
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Delete,
                motion_key: 'w',
                total_count: usize::MAX,
            })
        );
        // Same guard on the OpFind transition fold.
        let Outcome::Wait(folded) = step(state, Key::Char('f')) else {
            panic!("expected Wait(OpFind)");
        };
        assert_eq!(
            folded,
            PendingState::OpFind {
                op: OperatorKind::Delete,
                total_count: usize::MAX,
                forward: true,
                till: false,
            }
        );
    }

    // Per-operator round-trip tests.

    #[test]
    fn op_yank_doubled() {
        let state = after_op(OperatorKind::Yank, 1);
        assert_eq!(
            step(state, Key::Char('y')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Yank,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_change_doubled() {
        let state = after_op(OperatorKind::Change, 1);
        assert_eq!(
            step(state, Key::Char('c')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Change,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_indent_doubled() {
        let state = after_op(OperatorKind::Indent, 1);
        assert_eq!(
            step(state, Key::Char('>')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Indent,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_outdent_doubled() {
        let state = after_op(OperatorKind::Outdent, 1);
        assert_eq!(
            step(state, Key::Char('<')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Outdent,
                total_count: 1,
            })
        );
    }

    // ── New 2c-v operators: doubled-letter detection ─────────────────────────

    #[test]
    fn op_uppercase_then_cap_u_commits_double() {
        // AfterOp{Uppercase} + 'U' → ApplyOpDouble (gUU = uppercase current line)
        let state = after_op(OperatorKind::Uppercase, 1);
        assert_eq!(
            step(state, Key::Char('U')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Uppercase,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_lowercase_then_u_commits_double() {
        // AfterOp{Lowercase} + 'u' → ApplyOpDouble (guu = lowercase current line)
        let state = after_op(OperatorKind::Lowercase, 1);
        assert_eq!(
            step(state, Key::Char('u')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Lowercase,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_togglecase_then_tilde_commits_double() {
        // AfterOp{ToggleCase} + '~' → ApplyOpDouble (g~~ = toggle current line)
        let state = after_op(OperatorKind::ToggleCase, 1);
        assert_eq!(
            step(state, Key::Char('~')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::ToggleCase,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_reflow_then_q_commits_double() {
        // AfterOp{Reflow} + 'q' → ApplyOpDouble (gqq = reflow current line)
        let state = after_op(OperatorKind::Reflow, 1);
        assert_eq!(
            step(state, Key::Char('q')),
            Outcome::Commit(EngineCmd::ApplyOpDouble {
                op: OperatorKind::Reflow,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_uppercase_then_w_commits_motion() {
        // AfterOp{Uppercase} + 'w' → ApplyOpMotion (gUw = uppercase over word)
        let state = after_op(OperatorKind::Uppercase, 1);
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Uppercase,
                motion_key: 'w',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_reflow_then_ap_commits_text_obj() {
        // AfterOp{Reflow} + 'a' → Wait(OpTextObj{inner:false}) — verifies 'a'
        // transition works for Reflow (gqap = reflow around paragraph).
        let state = after_op(OperatorKind::Reflow, 1);
        let Outcome::Wait(obj_state) = step(state, Key::Char('a')) else {
            panic!("expected Wait(OpTextObj)");
        };
        assert_eq!(
            obj_state,
            PendingState::OpTextObj {
                op: OperatorKind::Reflow,
                total_count: 1,
                inner: false,
            }
        );
        // 'p' → commit ApplyOpTextObj
        assert_eq!(
            step(obj_state, Key::Char('p')),
            Outcome::Commit(EngineCmd::ApplyOpTextObj {
                op: OperatorKind::Reflow,
                ch: 'p',
                inner: false,
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_yank_motion() {
        let state = after_op(OperatorKind::Yank, 1);
        assert_eq!(
            step(state, Key::Char('$')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Yank,
                motion_key: '$',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_change_motion() {
        let state = after_op(OperatorKind::Change, 1);
        assert_eq!(
            step(state, Key::Char('w')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Change,
                motion_key: 'w',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_indent_motion() {
        let state = after_op(OperatorKind::Indent, 1);
        assert_eq!(
            step(state, Key::Char('j')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Indent,
                motion_key: 'j',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_outdent_motion() {
        let state = after_op(OperatorKind::Outdent, 1);
        assert_eq!(
            step(state, Key::Char('k')),
            Outcome::Commit(EngineCmd::ApplyOpMotion {
                op: OperatorKind::Outdent,
                motion_key: 'k',
                total_count: 1,
            })
        );
    }

    // ── OpG reducer unit tests ───────────────────────────────────────────────

    fn op_g(op: OperatorKind, total_count: usize) -> PendingState {
        PendingState::OpG { op, total_count }
    }

    #[test]
    fn op_d_then_g_then_g_commits_apply_op_g_for_gg() {
        // `dgg` → ApplyOpG { Delete, 'g', total_count:1 }
        let state = op_g(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('g')),
            Outcome::Commit(EngineCmd::ApplyOpG {
                op: OperatorKind::Delete,
                ch: 'g',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_g_then_e_commits_for_ge() {
        // `dge` → ApplyOpG { Delete, 'e', total_count:1 }
        let state = op_g(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('e')),
            Outcome::Commit(EngineCmd::ApplyOpG {
                op: OperatorKind::Delete,
                ch: 'e',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_d_then_g_then_j_commits_for_gj() {
        // `dgj` → ApplyOpG { Delete, 'j', total_count:1 }
        let state = op_g(OperatorKind::Delete, 1);
        assert_eq!(
            step(state, Key::Char('j')),
            Outcome::Commit(EngineCmd::ApplyOpG {
                op: OperatorKind::Delete,
                ch: 'j',
                total_count: 1,
            })
        );
    }

    #[test]
    fn op_2d_3g_g_total_count_6() {
        // `2d3gg`: count1=2, inner_count=3 → total=6 folded at AfterOp→OpG.
        // Simulate via AfterOp(count1=2, inner_count=3) then 'g', then 'g'.
        let state = PendingState::AfterOp {
            op: OperatorKind::Delete,
            count1: 2,
            inner_count: 3,
        };
        let Outcome::Wait(op_g_state) = step(state, Key::Char('g')) else {
            panic!("expected Wait(OpG)");
        };
        assert_eq!(
            op_g_state,
            PendingState::OpG {
                op: OperatorKind::Delete,
                total_count: 6,
            }
        );
        assert_eq!(
            step(op_g_state, Key::Char('g')),
            Outcome::Commit(EngineCmd::ApplyOpG {
                op: OperatorKind::Delete,
                ch: 'g',
                total_count: 6,
            })
        );
    }

    #[test]
    fn op_d_g_then_esc_cancels() {
        // `dg<Esc>` — Esc after OpG transition cancels.
        let state = op_g(OperatorKind::Delete, 1);
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn op_d_g_then_enter_cancels() {
        // Non-char key after `dg` cancels (mirrors OpFind / OpTextObj arms).
        let state = op_g(OperatorKind::Delete, 1);
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn op_c_then_g_then_g_commits_change_op_g() {
        // `cgg` → ApplyOpG { Change, 'g', total_count:1 }
        let state = op_g(OperatorKind::Change, 1);
        assert_eq!(
            step(state, Key::Char('g')),
            Outcome::Commit(EngineCmd::ApplyOpG {
                op: OperatorKind::Change,
                ch: 'g',
                total_count: 1,
            })
        );
    }

    // ── SelectRegister reducer unit tests ────────────────────────────────────

    #[test]
    fn select_register_a_commits() {
        // `"a` → SetPendingRegister { reg: 'a' }
        let state = PendingState::SelectRegister;
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::SetPendingRegister { reg: 'a' })
        );
    }

    #[test]
    fn select_register_plus_commits() {
        // `"+` → SetPendingRegister { reg: '+' } (system clipboard register)
        let state = PendingState::SelectRegister;
        assert_eq!(
            step(state, Key::Char('+')),
            Outcome::Commit(EngineCmd::SetPendingRegister { reg: '+' })
        );
    }

    #[test]
    fn select_register_underscore_commits() {
        // `"_` → SetPendingRegister { reg: '_' } (black-hole register)
        let state = PendingState::SelectRegister;
        assert_eq!(
            step(state, Key::Char('_')),
            Outcome::Commit(EngineCmd::SetPendingRegister { reg: '_' })
        );
    }

    #[test]
    fn select_register_esc_cancels() {
        let state = PendingState::SelectRegister;
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn select_register_enter_cancels() {
        // Non-char key after `"` cancels (engine FSM semantics: no-op = cancel).
        let state = PendingState::SelectRegister;
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── SetMark reducer unit tests ────────────────────────────────────────────

    #[test]
    fn set_mark_a_commits() {
        // `ma` → SetMark { ch: 'a' }
        let state = PendingState::SetMark;
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::SetMark { ch: 'a' })
        );
    }

    #[test]
    fn set_mark_esc_cancels() {
        let state = PendingState::SetMark;
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn set_mark_enter_cancels() {
        // Non-char key (Enter) after `m` cancels.
        let state = PendingState::SetMark;
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── GotoMarkLine reducer unit tests ───────────────────────────────────────

    #[test]
    fn goto_mark_line_a_commits() {
        // `'a` → GotoMarkLine { ch: 'a' }
        let state = PendingState::GotoMarkLine;
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::GotoMarkLine { ch: 'a' })
        );
    }

    #[test]
    fn goto_mark_line_esc_cancels() {
        let state = PendingState::GotoMarkLine;
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn goto_mark_line_enter_cancels() {
        // Non-char key (Enter) after `'` cancels.
        let state = PendingState::GotoMarkLine;
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── GotoMarkChar reducer unit tests ───────────────────────────────────────

    #[test]
    fn goto_mark_char_a_commits() {
        // `` `a `` → GotoMarkChar { ch: 'a' }
        let state = PendingState::GotoMarkChar;
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::GotoMarkChar { ch: 'a' })
        );
    }

    #[test]
    fn goto_mark_char_esc_cancels() {
        let state = PendingState::GotoMarkChar;
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn goto_mark_char_enter_cancels() {
        // Non-char key (Enter) after `` ` `` cancels.
        let state = PendingState::GotoMarkChar;
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    // ── RecordMacroTarget reducer unit tests ─────────────────────────────────

    #[test]
    fn record_macro_target_a_commits_start_record() {
        // `qa` → StartMacroRecord { reg: 'a' }
        let state = PendingState::RecordMacroTarget;
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::StartMacroRecord { reg: 'a' })
        );
    }

    #[test]
    fn record_macro_target_capital_a_commits_start_record() {
        // `qA` → StartMacroRecord { reg: 'A' } (capital = append to lowercase)
        let state = PendingState::RecordMacroTarget;
        assert_eq!(
            step(state, Key::Char('A')),
            Outcome::Commit(EngineCmd::StartMacroRecord { reg: 'A' })
        );
    }

    #[test]
    fn record_macro_target_esc_cancels() {
        let state = PendingState::RecordMacroTarget;
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn record_macro_target_enter_cancels() {
        // Non-char key (Enter) after `q` cancels.
        let state = PendingState::RecordMacroTarget;
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn record_macro_target_non_alnum_cancels() {
        // Non-alphabetic/digit char (e.g. '!') cancels.
        let state = PendingState::RecordMacroTarget;
        assert_eq!(step(state, Key::Char('!')), Outcome::Cancel);
    }

    // ── PlayMacroTarget reducer unit tests ───────────────────────────────────

    #[test]
    fn play_macro_target_a_commits_play() {
        // `@a` → PlayMacro { reg: 'a', count: 1 }
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::PlayMacro { reg: 'a', count: 1 })
        );
    }

    #[test]
    fn play_macro_target_at_sign_commits_play_with_at() {
        // `@@` → PlayMacro { reg: '@', count: 1 } (repeat-last semantics)
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(
            step(state, Key::Char('@')),
            Outcome::Commit(EngineCmd::PlayMacro { reg: '@', count: 1 })
        );
    }

    #[test]
    fn play_macro_target_with_count_3_preserves_count() {
        // `3@a` — count carried from the app's pending_count through BeginPendingPlayMacro.
        let state = PendingState::PlayMacroTarget { count: 3 };
        assert_eq!(
            step(state, Key::Char('a')),
            Outcome::Commit(EngineCmd::PlayMacro { reg: 'a', count: 3 })
        );
    }

    #[test]
    fn play_macro_target_esc_cancels() {
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(step(state, Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn play_macro_target_enter_cancels() {
        // Non-char key (Enter) after `@` cancels.
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(step(state, Key::Enter), Outcome::Cancel);
    }

    #[test]
    fn play_macro_target_non_alnum_cancels() {
        // Non-alphabetic/digit/@ char cancels (but ':' is a special case below).
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(step(state, Key::Char('!')), Outcome::Cancel);
    }

    #[test]
    fn play_macro_target_colon_commits_play_macro() {
        // `@:` → PlayMacro { reg: ':', count: 1 }. Host branches on reg == ':'
        // to replay the last ex command (Phase 5d, kryptic-sh/hjkl#71).
        let state = PendingState::PlayMacroTarget { count: 1 };
        assert_eq!(
            step(state, Key::Char(':')),
            Outcome::Commit(EngineCmd::PlayMacro { reg: ':', count: 1 })
        );
    }

    #[test]
    fn play_macro_target_colon_with_count_3_commits() {
        // `3@:` — count preserved through PlayMacroTarget.
        let state = PendingState::PlayMacroTarget { count: 3 };
        assert_eq!(
            step(state, Key::Char(':')),
            Outcome::Commit(EngineCmd::PlayMacro { reg: ':', count: 3 })
        );
    }
}
