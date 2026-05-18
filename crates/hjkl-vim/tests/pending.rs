use hjkl_vim::{EngineCmd, Key, OperatorKind, Outcome, PendingState, pending::step};

#[test]
fn replace_happy_path_commits_char() {
    let state = PendingState::Replace { count: 1 };
    let outcome = step(state, Key::Char('X'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::ReplaceChar { ch: 'X', count: 1 })
    );
}

#[test]
fn replace_esc_cancels() {
    let state = PendingState::Replace { count: 3 };
    let outcome = step(state, Key::Esc);
    assert_eq!(outcome, Outcome::Cancel);
}

#[test]
fn replace_preserves_count() {
    let state = PendingState::Replace { count: 5 };
    let outcome = step(state, Key::Char('Z'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::ReplaceChar { ch: 'Z', count: 5 })
    );
}

#[test]
fn replace_enter_becomes_newline() {
    let state = PendingState::Replace { count: 1 };
    let outcome = step(state, Key::Enter);
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::ReplaceChar { ch: '\n', count: 1 })
    );
}

#[test]
fn replace_backspace_cancels() {
    let state = PendingState::Replace { count: 2 };
    let outcome = step(state, Key::Backspace);
    assert_eq!(outcome, Outcome::Cancel);
}

#[test]
fn replace_tab_cancels() {
    let state = PendingState::Replace { count: 1 };
    let outcome = step(state, Key::Tab);
    assert_eq!(outcome, Outcome::Cancel);
}

// ── Find (f/F/t/T) tests ──────────────────────────────────────────────────

#[test]
fn find_forward_inclusive_commits() {
    // f<x> — forward, inclusive (f)
    let state = PendingState::Find {
        count: 1,
        forward: true,
        till: false,
    };
    let outcome = step(state, Key::Char('x'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::FindChar {
            ch: 'x',
            forward: true,
            till: false,
            count: 1
        })
    );
}

#[test]
fn find_backward_inclusive_commits() {
    // F<x> — backward, inclusive (F)
    let state = PendingState::Find {
        count: 1,
        forward: false,
        till: false,
    };
    let outcome = step(state, Key::Char('x'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::FindChar {
            ch: 'x',
            forward: false,
            till: false,
            count: 1
        })
    );
}

#[test]
fn find_forward_till_commits() {
    // t<x> — forward, till (t)
    let state = PendingState::Find {
        count: 2,
        forward: true,
        till: true,
    };
    let outcome = step(state, Key::Char('y'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::FindChar {
            ch: 'y',
            forward: true,
            till: true,
            count: 2
        })
    );
}

#[test]
fn find_backward_till_commits() {
    // T<x> — backward, till (T)
    let state = PendingState::Find {
        count: 3,
        forward: false,
        till: true,
    };
    let outcome = step(state, Key::Char('z'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::FindChar {
            ch: 'z',
            forward: false,
            till: true,
            count: 3
        })
    );
}

#[test]
fn find_esc_cancels() {
    let state = PendingState::Find {
        count: 1,
        forward: true,
        till: false,
    };
    let outcome = step(state, Key::Esc);
    assert_eq!(outcome, Outcome::Cancel);
}

#[test]
fn find_preserves_count() {
    let state = PendingState::Find {
        count: 7,
        forward: true,
        till: false,
    };
    let outcome = step(state, Key::Char('a'));
    assert_eq!(
        outcome,
        Outcome::Commit(EngineCmd::FindChar {
            ch: 'a',
            forward: true,
            till: false,
            count: 7
        })
    );
}

#[test]
fn find_enter_cancels() {
    // Non-char key (Enter) cancels find pending.
    let state = PendingState::Find {
        count: 1,
        forward: true,
        till: false,
    };
    let outcome = step(state, Key::Enter);
    assert_eq!(outcome, Outcome::Cancel);
}

// ── AutoIndent (=) operator pending tests ────────────────────────────────────

fn after_auto_indent(count1: usize) -> PendingState {
    PendingState::AfterOp {
        op: OperatorKind::AutoIndent,
        count1,
        inner_count: 0,
    }
}

#[test]
fn op_auto_indent_then_equal_commits_double() {
    // `==` → AfterOp{AutoIndent} + '=' → ApplyOpDouble (reindent current line)
    let state = after_auto_indent(1);
    assert_eq!(
        step(state, Key::Char('=')),
        Outcome::Commit(EngineCmd::ApplyOpDouble {
            op: OperatorKind::AutoIndent,
            total_count: 1,
        })
    );
}

#[test]
fn op_auto_indent_then_motion_commits_motion() {
    // `=j` → AfterOp{AutoIndent} + 'j' → ApplyOpMotion
    let state = after_auto_indent(1);
    assert_eq!(
        step(state, Key::Char('j')),
        Outcome::Commit(EngineCmd::ApplyOpMotion {
            op: OperatorKind::AutoIndent,
            motion_key: 'j',
            total_count: 1,
        })
    );
}
