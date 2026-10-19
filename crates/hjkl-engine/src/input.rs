//! Backend-agnostic key input types used by the vim engine.
//!
//! Phase 8 of the hjkl-buffer migration replaced `tui_textarea::Input`
//! / `tui_textarea::Key` with these in-crate equivalents so the
//! editor can drop the `tui-textarea` dependency entirely.

/// A key code, mirroring the subset of [`crossterm::event::KeyCode`]
/// the vim engine actually consumes. `Null` is the conventional
/// sentinel for "no input" (matching the previous `tui_textarea::Key`
/// shape) so call sites can early-return on unsupported keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Key {
    Char(char),
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Tab,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Esc,
    #[default]
    Null,
}

/// A key press with modifier flags. The vim engine reads modifiers
/// directly off this struct (e.g. `input.ctrl && input.key == Key::Char('d')`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Serialize a captured macro into vim's keystroke notation
/// (`<Esc>`, `<C-d>`, `<lt>`, etc.) so it can live as plain text in a
/// register slot. Used when `q{reg}` finishes recording.
pub fn encode_macro(inputs: &[Input]) -> String {
    let mut out = String::new();
    for input in inputs {
        match input.key {
            Key::Char(c) if input.ctrl => {
                out.push_str("<C-");
                out.push(c);
                out.push('>');
            }
            Key::Char(c) if input.alt => {
                out.push_str("<M-");
                out.push(c);
                out.push('>');
            }
            Key::Char('<') => out.push_str("<lt>"),
            Key::Char(c) => out.push(c),
            Key::Esc => out.push_str("<Esc>"),
            Key::Enter => out.push_str("<CR>"),
            Key::Backspace => out.push_str("<BS>"),
            Key::Tab => out.push_str("<Tab>"),
            Key::Up => out.push_str("<Up>"),
            Key::Down => out.push_str("<Down>"),
            Key::Left => out.push_str("<Left>"),
            Key::Right => out.push_str("<Right>"),
            Key::Delete => out.push_str("<Del>"),
            Key::Home => out.push_str("<Home>"),
            Key::End => out.push_str("<End>"),
            Key::PageUp => out.push_str("<PageUp>"),
            Key::PageDown => out.push_str("<PageDown>"),
            Key::Null => {}
        }
    }
    out
}

/// The longest tag body [`decode_macro`] recognises (`PageDown`). A `>`
/// further away than this cannot close a known tag, so the search for it
/// stops here and decoding stays linear in the input.
const LONGEST_TAG: usize = "PageDown".len();

/// Reverse of [`encode_macro`] — parse the textual form back into
/// `Input` events for replay. A `<` that does not open a known `<…>` tag is
/// a literal `<`, and scanning resumes right after it, as vim's notation
/// parser does: `<<Esc>` is `<` then Escape, and an unknown `<xyz>` replays
/// as its literal characters, so pasted register text round-trips without
/// silently dropping user content.
pub fn decode_macro(s: &str) -> Vec<Input> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == '<' {
            let rest = &chars[i..];
            let close = rest.iter().take(LONGEST_TAG + 1).position(|&ch| ch == '>');
            if let Some(len) = close
                && let Some(input) = key_from_tag(&rest[..len].iter().collect::<String>())
            {
                out.push(input);
                i += len + 1;
                continue;
            }
        }
        out.push(Input {
            key: Key::Char(c),
            ..Input::default()
        });
    }
    out
}

/// The key a `<tag>` body names, or `None` when it names none.
fn key_from_tag(tag: &str) -> Option<Input> {
    let key = match tag {
        "Esc" => Key::Esc,
        "CR" => Key::Enter,
        "BS" => Key::Backspace,
        "Tab" => Key::Tab,
        "Space" => Key::Char(' '),
        "Up" => Key::Up,
        "Down" => Key::Down,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Del" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "lt" => Key::Char('<'),
        t => {
            let mut parts = t.chars();
            let (modifier, dash, ch) = (parts.next()?, parts.next()?, parts.next()?);
            if dash != '-' || parts.next().is_some() {
                return None;
            }
            return match modifier {
                'C' => Some(Input {
                    key: Key::Char(ch),
                    ctrl: true,
                    ..Input::default()
                }),
                'M' => Some(Input {
                    key: Key::Char(ch),
                    alt: true,
                    ..Input::default()
                }),
                _ => None,
            };
        }
    };
    Some(Input {
        key,
        ..Input::default()
    })
}

/// Decode a [`crate::types::Input`] (alias: [`crate::PlannedInput`]) to the
/// engine-internal [`Input`] type.
///
/// Returns `None` for variants the legacy FSM does not dispatch
/// (`Mouse`, `Paste`, `FocusGained`, `FocusLost`, `Resize`) and for any
/// special-key variant that maps to [`Key::Null`] (e.g. `SpecialKey::Insert`,
/// `SpecialKey::F(_)`).
///
/// Phase 6.6g.1: extracted from `Editor::feed_input` so that both the
/// engine-internal path and `hjkl_vim::feed_input` share the same decode
/// without duplication.
pub fn from_planned(planned: crate::types::Input) -> Option<Input> {
    use crate::types::{Input as PlannedInput, SpecialKey};
    let (key, mods) = match planned {
        PlannedInput::Char(c, m) => (Key::Char(c), m),
        PlannedInput::Key(k, m) => {
            let key = match k {
                SpecialKey::Esc => Key::Esc,
                SpecialKey::Enter => Key::Enter,
                SpecialKey::Backspace => Key::Backspace,
                SpecialKey::Tab => Key::Tab,
                // Engine's internal `Key` doesn't model BackTab as a
                // distinct variant — fall through to the FSM as
                // shift+Tab, matching crossterm semantics.
                SpecialKey::BackTab => Key::Tab,
                SpecialKey::Up => Key::Up,
                SpecialKey::Down => Key::Down,
                SpecialKey::Left => Key::Left,
                SpecialKey::Right => Key::Right,
                SpecialKey::Home => Key::Home,
                SpecialKey::End => Key::End,
                SpecialKey::PageUp => Key::PageUp,
                SpecialKey::PageDown => Key::PageDown,
                // Engine's `Key` has no Insert / F(n) — drop to Null
                // (FSM ignores it) which matches the crossterm path
                // (`crossterm_to_input` mapped these to Null too).
                SpecialKey::Insert => Key::Null,
                SpecialKey::Delete => Key::Delete,
                SpecialKey::F(_) => Key::Null,
            };
            let m = if matches!(k, SpecialKey::BackTab) {
                crate::types::Modifiers { shift: true, ..m }
            } else {
                m
            };
            (key, m)
        }
        // Variants the legacy FSM doesn't consume yet.
        PlannedInput::Mouse(_)
        | PlannedInput::Paste(_)
        | PlannedInput::FocusGained
        | PlannedInput::FocusLost
        | PlannedInput::Resize(_, _) => return None,
    };
    if key == Key::Null {
        return None;
    }
    Some(Input {
        key,
        ctrl: mods.ctrl,
        alt: mods.alt,
        shift: mods.shift,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple_chars() {
        let keys = vec![
            Input {
                key: Key::Char('h'),
                ..Input::default()
            },
            Input {
                key: Key::Char('i'),
                ..Input::default()
            },
        ];
        let text = encode_macro(&keys);
        assert_eq!(text, "hi");
        assert_eq!(decode_macro(&text), keys);
    }

    #[test]
    fn roundtrip_with_special_keys_and_ctrl() {
        let keys = vec![
            Input {
                key: Key::Char('i'),
                ..Input::default()
            },
            Input {
                key: Key::Char('X'),
                ..Input::default()
            },
            Input {
                key: Key::Esc,
                ..Input::default()
            },
            Input {
                key: Key::Char('d'),
                ctrl: true,
                ..Input::default()
            },
        ];
        let text = encode_macro(&keys);
        assert_eq!(text, "iX<Esc><C-d>");
        assert_eq!(decode_macro(&text), keys);
    }

    #[test]
    fn roundtrip_literal_lt() {
        let keys = vec![Input {
            key: Key::Char('<'),
            ..Input::default()
        }];
        let text = encode_macro(&keys);
        assert_eq!(text, "<lt>");
        assert_eq!(decode_macro(&text), keys);
    }

    /// B15: an unrecognized `<...>` tag must replay as its literal characters
    /// (`<`, tag body, `>`) rather than vanishing — mirrors the existing
    /// unterminated-`<foo` handling just above it.
    #[test]
    fn unknown_tag_replays_literally() {
        fn ch(c: char) -> Input {
            Input {
                key: Key::Char(c),
                ..Input::default()
            }
        }
        let decoded = decode_macro("<xyz>ok");
        let expected = vec![
            ch('<'),
            ch('x'),
            ch('y'),
            ch('z'),
            ch('>'),
            ch('o'),
            ch('k'),
        ];
        assert_eq!(decoded, expected);
    }

    /// `<Space>` is a space, as `nvim_input` reads it.
    #[test]
    fn space_tag_decodes_to_a_space() {
        let space = Input {
            key: Key::Char(' '),
            ..Input::default()
        };
        assert_eq!(decode_macro("<Space>"), vec![space]);
    }

    /// A `<` that opens no known tag is literal and scanning resumes right
    /// after it, so a known tag nested inside the would-be tag still decodes:
    /// `<<$<Esc>` is `<`, `<`, `$`, Escape, as nvim reads it.
    #[test]
    fn literal_lt_rescans_for_a_nested_tag() {
        fn ch(c: char) -> Input {
            Input {
                key: Key::Char(c),
                ..Input::default()
            }
        }
        let esc = Input {
            key: Key::Esc,
            ..Input::default()
        };
        assert_eq!(
            decode_macro("<<$<Esc>"),
            vec![ch('<'), ch('<'), ch('$'), esc]
        );
        assert_eq!(decode_macro("<<Esc>"), vec![ch('<'), esc]);
        assert_eq!(decode_macro("<ab"), vec![ch('<'), ch('a'), ch('b')]);
        assert_eq!(
            decode_macro("<C-ab>"),
            "<C-ab>".chars().map(ch).collect::<Vec<_>>()
        );
    }

    /// Round-trip sanity: encode/decode must still agree after the fix.
    #[test]
    fn roundtrip_still_holds_after_unknown_tag_fix() {
        let keys = vec![
            Input {
                key: Key::Char('d'),
                ctrl: true,
                ..Input::default()
            },
            Input {
                key: Key::Esc,
                ..Input::default()
            },
        ];
        let text = encode_macro(&keys);
        assert_eq!(decode_macro(&text), keys);
    }
}
