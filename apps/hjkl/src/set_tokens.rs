//! Shared host-owned `:set` token interception.
//!
//! `mouse`, `endofline` and `explorer.open` are host state, not engine options
//! — hjkl's option registry rejects them. Three dispatch paths run ex
//! commands (the TUI's `App::dispatch_ex`, `--headless`'s run loop, `--embed`'s
//! `hjkl_command`), so all three run the same interception through a
//! [`SetHost`] impl; the impl decides what each token means in that mode.
//!
//! Persistence is deliberately TUI-only: the TUI writes a clean `:set` back to
//! the user's config file (`App::persist_set_options`), while `--headless` and
//! `--embed` load no config file at all, so their `:set` applies to the
//! session only — a stated decision, not an oversight.

use crate::save::EolState;

/// Outcome of intercepting the tokens of one `:set` line.
#[derive(Debug, PartialEq, Eq)]
pub enum SetLine {
    /// No host token was present — dispatch the line unchanged.
    PassThrough,
    /// Every token was consumed — swallow the whole line (nothing to dispatch).
    Swallow,
    /// Some tokens were consumed — dispatch this rebuilt `set …` line instead.
    Rebuild(String),
}

/// Host state a `:set` token can mutate, one impl per mode.
pub trait SetHost {
    /// Handle one `mouse`-family token (`mouse`, `nomouse`, `mouse!`,
    /// `mouse?`, `mouse=…`). True = consumed. The TUI updates capture +
    /// per-mode flags and answers `mouse?` via [`Self::info`]; headless/embed
    /// (no terminal) consume silently.
    fn set_mouse(&mut self, token: &str) -> bool;
    /// Handle one `explorer.open` token (`explorer.open?`, `explorer.open=…`).
    /// True = consumed. The TUI reads/writes the dock preference and answers
    /// queries / errors via [`Self::info`] / [`Self::error`]; headless/embed
    /// (no explorer) consume silently.
    fn set_explorer_open(&mut self, token: &str) -> bool;
    /// Apply endofline tokens to the mode's `EolState`, returning the leftover
    /// tokens and the info replies to surface. Implement with
    /// `crate::save::take_endofline_tokens`.
    fn set_endofline<'a>(&mut self, tokens: Vec<&'a str>) -> (Vec<&'a str>, Vec<String>);
    /// Surface an info reply. Default no-op (headless/embed suppress Info).
    fn info(&mut self, _msg: String) {}
    /// Surface an error reply. Default no-op.
    fn error(&mut self, _msg: String) {}
}

/// Intercept the host-owned tokens of a `set` line. `body` is the text AFTER
/// the leading `set ` (trimmed), exactly as the TUI splits it. Preserves the
/// TUI's order: mouse + explorer.open are matched in one pass over the tokens,
/// endofline runs on the remainder. Returns what to do with the line.
pub fn intercept_set_tokens(body: &str, host: &mut impl SetHost) -> SetLine {
    let mut remaining: Vec<&str> = Vec::new();
    let mut consumed_any = false;
    for tok in body.split_whitespace() {
        if host.set_mouse(tok) || host.set_explorer_open(tok) {
            consumed_any = true;
        } else {
            remaining.push(tok);
        }
    }
    let before = remaining.len();
    let (rest, replies) = host.set_endofline(remaining);
    for reply in replies {
        host.info(reply);
    }
    consumed_any |= rest.len() != before;
    if !consumed_any {
        SetLine::PassThrough
    } else if rest.is_empty() {
        SetLine::Swallow
    } else {
        SetLine::Rebuild(format!("set {}", rest.join(" ")))
    }
}

/// [`SetHost`] for the non-TUI modes: no terminal, no explorer, no config
/// file — mouse and explorer.open tokens are consumed as no-ops, info/error
/// replies are dropped (headless/embed suppress Info output).
pub struct NonTuiSetHost<'a> {
    pub eol: &'a mut EolState,
}

impl SetHost for NonTuiSetHost<'_> {
    // Recognise the mouse / explorer.open families exactly like the TUI impl
    // does, but without the side effects: headless/embed have no terminal
    // capture to toggle and no explorer to open, so the tokens are no-ops.
    fn set_mouse(&mut self, token: &str) -> bool {
        token == "mouse"
            || token == "nomouse"
            || token == "mouse!"
            || token == "mouse?"
            || token.starts_with("mouse=")
    }

    fn set_explorer_open(&mut self, token: &str) -> bool {
        token == "explorer.open?" || token.starts_with("explorer.open=")
    }

    fn set_endofline<'a>(&mut self, tokens: Vec<&'a str>) -> (Vec<&'a str>, Vec<String>) {
        crate::save::take_endofline_tokens(tokens.into_iter(), self.eol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain engine-owned tokens are not host tokens — the line passes
    /// through untouched, exactly what the TUI dispatches to hjkl-ex.
    #[test]
    fn plain_tokens_pass_through() {
        let mut eol = EolState::default();
        let mut host = NonTuiSetHost { eol: &mut eol };
        assert_eq!(intercept_set_tokens("nu", &mut host), SetLine::PassThrough);
        assert_eq!(
            intercept_set_tokens("ts=4 nu", &mut host),
            SetLine::PassThrough
        );
    }

    /// A line made up entirely of mouse/explorer.open tokens is fully
    /// consumed: nothing reaches hjkl-ex, which would reject them.
    #[test]
    fn mouse_and_explorer_lines_are_swallowed() {
        let mut eol = EolState::default();
        let mut host = NonTuiSetHost { eol: &mut eol };
        assert_eq!(intercept_set_tokens("mouse=a", &mut host), SetLine::Swallow);
        assert_eq!(
            intercept_set_tokens("explorer.open=true", &mut host),
            SetLine::Swallow
        );
        assert_eq!(
            intercept_set_tokens("mouse=a explorer.open=true", &mut host),
            SetLine::Swallow
        );
    }

    /// Host tokens mixed with engine tokens rebuild the line so the engine
    /// tokens still apply (`:set nu nomouse` must turn `number` on).
    #[test]
    fn mixed_lines_are_rebuilt() {
        let mut eol = EolState::default();
        let mut host = NonTuiSetHost { eol: &mut eol };
        assert_eq!(
            intercept_set_tokens("mouse=a nu", &mut host),
            SetLine::Rebuild("set nu".to_string())
        );
        assert_eq!(
            intercept_set_tokens("explorer.open=false ts=8", &mut host),
            SetLine::Rebuild("set ts=8".to_string())
        );
    }

    /// endofline tokens mutate the host's `EolState` and leave the rest of
    /// the line to dispatch; a pure-endofline line is swallowed.
    #[test]
    fn endofline_tokens_apply_to_eol_state() {
        let mut eol = EolState::default();
        let mut host = NonTuiSetHost { eol: &mut eol };
        assert_eq!(
            intercept_set_tokens("endofline nu", &mut host),
            SetLine::Rebuild("set nu".to_string())
        );
        assert!(host.eol.endofline);
        assert_eq!(intercept_set_tokens("noeol", &mut host), SetLine::Swallow);
        assert!(
            !host.eol.endofline,
            "`noeol` must flip the host's EolState, not just be swallowed"
        );
    }
}
