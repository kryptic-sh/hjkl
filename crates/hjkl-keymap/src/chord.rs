//! Vim-style chord notation parser and serializer.
//!
//! A [`Chord`] is an ordered sequence of [`KeyEvent`]s that the user must
//! type in sequence to trigger a binding — e.g. `<leader>gs`, `<C-w>h`, `gd`.

use std::fmt;

use crate::key::{KeyCode, KeyEvent, KeyModifiers};
use thiserror::Error;

/// An ordered sequence of key events forming a multi-key binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord(pub Vec<KeyEvent>);

impl Chord {
    /// Construct a chord from a slice of key events.
    pub fn from_events(events: impl Into<Vec<KeyEvent>>) -> Self {
        Self(events.into())
    }

    /// Number of key events in this chord.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there are no key events.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parse a vim-style notation string into a chord.
    ///
    /// `leader` is the character that `<leader>` expands to (e.g. `' '`).
    ///
    /// Recognized tokens:
    /// - `<leader>` → expands to the supplied `leader` char (no modifiers)
    /// - `<C-x>`, `<S-x>`, `<A-x>`, `<C-S-x>`, `<C-A-x>`, `<S-A-x>`
    /// - `<Esc>`, `<CR>`, `<Tab>`, `<BS>`, `<Del>`, `<Insert>`
    /// - `<Up>`, `<Down>`, `<Left>`, `<Right>`
    /// - `<Home>`, `<End>`, `<PageUp>`, `<PageDown>`
    /// - `<F1>`–`<F12>`
    /// - `<Space>` → `Char(' ')`
    /// - `<lt>` → `Char('<')`
    /// - `<gt>` → `Char('>')` (cosmetic symmetry with `<lt>`; bare `>` also
    ///   works since `>` isn't tag-significant outside a closing context)
    /// - Bare characters → `Char(c)` with no modifiers
    pub fn parse(s: &str, leader: char) -> Result<Self, ChordParseError> {
        let mut events = Vec::new();
        let mut chars = s.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '<' {
                // Collect everything up to the matching `>`.
                let mut tag = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '>' {
                        closed = true;
                        break;
                    }
                    tag.push(next);
                }
                if !closed {
                    return Err(ChordParseError::UnclosedAngle(format!("<{tag}")));
                }
                let ev = parse_angle_tag(&tag, leader)?;
                events.push(ev);
            } else {
                events.push(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
            }
        }

        Ok(Self(events))
    }

    /// Render the chord back to vim-style notation.
    ///
    /// `leader` is the char that was used as the leader expansion so we can
    /// represent the leader key as `<leader>` when it appears.
    pub fn to_notation(&self, leader: char) -> String {
        let mut out = String::new();
        for ev in &self.0 {
            out.push_str(&event_to_notation(ev, leader));
        }
        out
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Display uses a placeholder leader char since we don't have context.
        write!(f, "{}", self.to_notation(' '))
    }
}

/// Error returned when parsing a chord notation string fails.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChordParseError {
    #[error("unclosed angle bracket: {0}")]
    UnclosedAngle(String),
    #[error("unknown special key: <{0}>")]
    UnknownSpecial(String),
    #[error("modifier tag requires a single character: <{0}>")]
    BadModifierTarget(String),
}

/// Parse a single `<...>` tag (the content between `<` and `>`).
fn parse_angle_tag(tag: &str, leader: char) -> Result<KeyEvent, ChordParseError> {
    let lower = tag.to_ascii_lowercase();

    // ── Named specials ────────────────────────────────────────────────────
    match lower.as_str() {
        "leader" => return Ok(KeyEvent::new(KeyCode::Char(leader), KeyModifiers::NONE)),
        "space" => {
            return Ok(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        }
        "lt" => {
            return Ok(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE));
        }
        "gt" => {
            return Ok(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE));
        }
        "cr" | "enter" | "return" => {
            return Ok(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        "esc" | "escape" => {
            return Ok(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        "tab" => {
            return Ok(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        "bs" | "backspace" => {
            return Ok(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        "del" | "delete" => {
            return Ok(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        }
        "insert" | "ins" => {
            return Ok(KeyEvent::new(KeyCode::Insert, KeyModifiers::NONE));
        }
        "up" => {
            return Ok(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        "down" => {
            return Ok(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        "left" => {
            return Ok(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        "right" => {
            return Ok(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        "home" => {
            return Ok(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        }
        "end" => {
            return Ok(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        }
        "pageup" => {
            return Ok(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        }
        "pagedown" => {
            return Ok(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        _ => {}
    }

    // ── F-keys ────────────────────────────────────────────────────────────
    if let Some(rest) = lower.strip_prefix('f')
        && let Ok(n) = rest.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Ok(KeyEvent::new(KeyCode::F(n), KeyModifiers::NONE));
    }

    // ── Modifier combos: C-, S-, A-, and combinations ────────────────────
    parse_modifier_tag(tag)
}

/// Parse a modifier-prefix tag such as `C-x`, `S-Tab`, `C-S-x`, `C-A-x`.
/// `tag` is the original (case-preserving) content between `<` and `>`.
fn parse_modifier_tag(tag: &str) -> Result<KeyEvent, ChordParseError> {
    // Split by `-`, treating everything after the last known modifier prefix
    // as the key name.
    let mut parts: Vec<&str> = tag.split('-').collect();
    if parts.is_empty() {
        return Err(ChordParseError::UnknownSpecial(tag.to_string()));
    }

    let mut modifiers = KeyModifiers::NONE;
    // Consume leading modifier prefixes (C, S, A/M), case-insensitive.
    while parts.len() > 1 {
        match parts[0].to_ascii_uppercase().as_str() {
            "C" => {
                modifiers |= KeyModifiers::CTRL;
                parts.remove(0);
            }
            "S" => {
                modifiers |= KeyModifiers::SHIFT;
                parts.remove(0);
            }
            "A" | "M" => {
                modifiers |= KeyModifiers::ALT;
                parts.remove(0);
            }
            _ => break,
        }
    }

    if modifiers.is_empty() {
        return Err(ChordParseError::UnknownSpecial(tag.to_string()));
    }

    // What remains in parts is the key name.
    let key_name = parts.join("-");
    let code = parse_key_name(&key_name, tag)?;
    Ok(KeyEvent::new(code, modifiers))
}

/// Resolve a key name (the part after all modifiers) to a [`KeyCode`].
fn parse_key_name(name: &str, original_tag: &str) -> Result<KeyCode, ChordParseError> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "space" => return Ok(KeyCode::Char(' ')),
        "lt" => return Ok(KeyCode::Char('<')),
        "gt" => return Ok(KeyCode::Char('>')),
        "cr" | "enter" | "return" => return Ok(KeyCode::Enter),
        "esc" | "escape" => return Ok(KeyCode::Esc),
        "tab" => return Ok(KeyCode::Tab),
        "bs" | "backspace" => return Ok(KeyCode::Backspace),
        "del" | "delete" => return Ok(KeyCode::Delete),
        "insert" | "ins" => return Ok(KeyCode::Insert),
        "up" => return Ok(KeyCode::Up),
        "down" => return Ok(KeyCode::Down),
        "left" => return Ok(KeyCode::Left),
        "right" => return Ok(KeyCode::Right),
        "home" => return Ok(KeyCode::Home),
        "end" => return Ok(KeyCode::End),
        "pageup" => return Ok(KeyCode::PageUp),
        "pagedown" => return Ok(KeyCode::PageDown),
        _ => {}
    }
    // F-key?
    if let Some(rest) = lower.strip_prefix('f')
        && let Ok(n) = rest.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Ok(KeyCode::F(n));
    }
    // Single character?
    let mut chars = name.chars();
    let first = chars.next();
    let second = chars.next();
    match (first, second) {
        (Some(c), None) => Ok(KeyCode::Char(c)),
        _ => Err(ChordParseError::BadModifierTarget(original_tag.to_string())),
    }
}

/// Render a single [`KeyEvent`] to vim-style notation.
pub(crate) fn event_to_notation(ev: &KeyEvent, leader: char) -> String {
    // Special-case: leader key with no modifiers.
    if ev.modifiers == KeyModifiers::NONE
        && let KeyCode::Char(c) = ev.code
        && c == leader
    {
        return "<leader>".to_string();
    }

    let has_ctrl = ev.modifiers.contains(KeyModifiers::CTRL);
    let has_shift = ev.modifiers.contains(KeyModifiers::SHIFT);
    let has_alt = ev.modifiers.contains(KeyModifiers::ALT);
    let has_any_mod = has_ctrl || has_shift || has_alt;

    let key_str = match ev.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char('<') => "lt".to_string(),
        // Inside a modifier tag a bare '>' would close the tag early
        // (`<C->>` is unparseable), so escape it as `gt`.
        KeyCode::Char('>') => "gt".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "CR".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Backspace => "BS".to_string(),
        KeyCode::Delete => "Del".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::F(n) => format!("F{n}"),
    };

    if !has_any_mod {
        // Plain character — no angle brackets needed for printable non-special chars.
        match ev.code {
            KeyCode::Char(' ') => return "<Space>".to_string(),
            KeyCode::Char('<') => return "<lt>".to_string(),
            KeyCode::Char(c) => return c.to_string(),
            _ => return format!("<{key_str}>"),
        }
    }

    let mut prefix = String::new();
    if has_ctrl {
        prefix.push_str("C-");
    }
    if has_shift {
        prefix.push_str("S-");
    }
    if has_alt {
        prefix.push_str("A-");
    }
    format!("<{prefix}{key_str}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_chars() {
        let chord = Chord::parse("gd", ' ').unwrap();
        assert_eq!(chord.0.len(), 2);
        assert_eq!(
            chord.0[0],
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)
        );
        assert_eq!(
            chord.0[1],
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn parse_leader() {
        let chord = Chord::parse("<leader>gs", ' ').unwrap();
        assert_eq!(chord.0.len(), 3);
        assert_eq!(
            chord.0[0],
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)
        );
    }

    #[test]
    fn parse_ctrl() {
        let chord = Chord::parse("<C-w>h", ' ').unwrap();
        assert_eq!(
            chord.0[0],
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL)
        );
        assert_eq!(
            chord.0[1],
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn parse_lt_gt_escapes() {
        // <lt> → literal '<' (must escape because bare '<' starts a tag)
        let chord = Chord::parse("<C-w><lt>", ' ').unwrap();
        assert_eq!(
            chord.0,
            vec![
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL),
                KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE),
            ]
        );

        // <gt> → literal '>' (symmetry; bare '>' also works outside a tag)
        let chord = Chord::parse("<C-w><gt>", ' ').unwrap();
        assert_eq!(
            chord.0,
            vec![
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL),
                KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE),
            ]
        );

        // Bare '>' still parses as Char('>') for backward-compat.
        let chord = Chord::parse("<C-w>>", ' ').unwrap();
        assert_eq!(
            chord.0,
            vec![
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CTRL),
                KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE),
            ]
        );
    }

    #[test]
    fn parse_shift_tab() {
        let chord = Chord::parse("<S-Tab>", ' ').unwrap();
        assert_eq!(chord.0[0], KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    }

    #[test]
    fn parse_ctrl_shift() {
        let chord = Chord::parse("<C-S-Tab>", ' ').unwrap();
        let expected_mods = KeyModifiers::CTRL | KeyModifiers::SHIFT;
        assert_eq!(chord.0[0], KeyEvent::new(KeyCode::Tab, expected_mods));
    }

    #[test]
    fn round_trip_leader_gs() {
        let leader = ' ';
        let input = "<leader>gs";
        let chord = Chord::parse(input, leader).unwrap();
        let output = chord.to_notation(leader);
        assert_eq!(output, input);
    }

    #[test]
    fn round_trip_ctrl_shift_tab() {
        let leader = ' ';
        let chord = Chord::parse("<C-S-Tab>", leader).unwrap();
        let output = chord.to_notation(leader);
        assert_eq!(output, "<C-S-Tab>");
    }

    #[test]
    fn parse_modifier_with_gt_and_lt() {
        // `gt`/`lt` must work as modifier targets too — a bare '>' would
        // close the tag early and a bare '<' would open a new one.
        let chord = Chord::parse("<C-gt><C-lt>", ' ').unwrap();
        assert_eq!(
            chord.0,
            vec![
                KeyEvent::new(KeyCode::Char('>'), KeyModifiers::CTRL),
                KeyEvent::new(KeyCode::Char('<'), KeyModifiers::CTRL),
            ]
        );
    }

    #[test]
    fn round_trip_modified_gt() {
        // Ctrl+'>' must not serialize as `<C->>` (unparseable).
        let chord = Chord(vec![KeyEvent::new(KeyCode::Char('>'), KeyModifiers::CTRL)]);
        let notation = chord.to_notation(' ');
        assert_eq!(notation, "<C-gt>");
        assert_eq!(Chord::parse(&notation, ' ').unwrap(), chord);
    }

    #[test]
    fn unclosed_angle_error() {
        let result = Chord::parse("<C-w", ' ');
        assert!(matches!(result, Err(ChordParseError::UnclosedAngle(_))));
    }
}
