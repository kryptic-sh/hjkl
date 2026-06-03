//! E2e: explorer `/` opens the fuzzy-search prompt (real pty render check).

use super::harness::TerminalSession;

/// `<leader>e` opens the explorer; `/` must open the explorer fuzzy-search
/// prompt at the bottom status row (a leading `/` becomes visible).
#[test]
fn explorer_slash_shows_search_prompt() {
    let mut s = TerminalSession::spawn();

    // Open the explorer (leader = space), then `/` opens its fuzzy-search
    // prompt — proving `/` routes through the per-buffer search override at
    // runtime (regression guard for the keymap-bypass bug). Typing/filtering
    // is covered by the unit test `slash_opens_search_typing_filters_esc_cancels`
    // (the tree walk here roots at the harness cwd, which is env-dependent).
    s.keys("<Space>e");
    s.keys("/");

    let bottom: String = (20..24).map(|r| s.line(r)).collect::<Vec<_>>().join("\n");
    let dump: String = (0..24)
        .map(|r| format!("{r:>2}|{}", s.line(r)))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bottom.lines().any(|l| l.trim_start().starts_with('/')),
        "after `/` the explorer fuzzy-search prompt (leading `/`) must show.\n{dump}"
    );
}
